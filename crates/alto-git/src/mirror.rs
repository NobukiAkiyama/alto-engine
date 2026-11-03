//! Maps the Alto commit DAG and its references onto Git commits and refs.
//!
//! One Alto commit is one Git commit, with the same parents in the same order. A
//! state blob at `refs/alto/<document>/state` records the commit table and the value
//! each mapped ref had after the last sync. Every recorded fact is checked before
//! anything is written, and all refs move in one `update-ref` transaction with their
//! expected old values, so a sync either lands whole or leaves every ref as it was.
use crate::{Opened, git, git_bytes, intent_lines, referenced, short};
use alto_core::ContentHash;
use alto_doc::{Commit, Document, NodeKind};
use alto_store::folder;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::Result;

const STATE_FORMAT: u32 = 1;
const DEFAULT_REF_PREFIX: &str = "alto";

#[derive(Clone, Copy, Debug, Default)]
pub struct SyncOptions<'a> {
    /// Git branch for the first-mapped Alto branch. Defaults to the checked-out branch.
    pub branch: Option<&'a str>,
    /// Namespace for every other Alto branch and each tag. Defaults to `alto`.
    pub ref_prefix: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RefUpdate {
    pub git_ref: String,
    /// The new Git commit, or None when the ref was deleted.
    pub commit: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SyncReport {
    pub created: Vec<(ContentHash, String)>,
    pub branch: String,
    pub refs: Vec<RefUpdate>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefEntry {
    git_ref: String,
    alto: ContentHash,
    git: String,
    /// False for a Git branch that existed before the first sync. Such a ref
    /// belongs to the repository, so deleting the Alto branch does not delete it.
    created: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncState {
    format: u32,
    document: String,
    prefix: String,
    ref_prefix: String,
    /// The tip the first sync built on. Alto root commits take it as their parent.
    base: Option<String>,
    trunk_alto: Option<String>,
    trunk_ref: String,
    commits: BTreeMap<ContentHash, String>,
    branches: BTreeMap<String, RefEntry>,
    tags: BTreeMap<String, RefEntry>,
    /// Pinned commits and a detached head, kept reachable in the private namespace.
    keep: BTreeMap<ContentHash, String>,
}

fn conflict(message: impl std::fmt::Display) -> String {
    format!("CONFLICT: {message}")
}

/// Runs git with `input` on stdin. The writer has its own thread because `cat-file
/// --batch` fills its stdout while it is still reading.
fn git_stdin(dir: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git を実行できません: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("git stdin")?;
    let out = std::thread::scope(|scope| {
        scope.spawn(move || stdin.write_all(input));
        child.wait_with_output()
    })
    .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn rev_parse(root: &Path, reference: &str) -> Option<String> {
    git(root, &["rev-parse", "--verify", "--quiet", reference])
        .ok()
        .map(|s| s.trim().to_string())
}

fn path_token(label: &str, value: &str) -> Result<()> {
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!("{label} must not contain whitespace"));
    }
    Ok(())
}
fn reject_damaged_commits(entries: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    for (name, bytes) in entries {
        if let Some(rest) = name.strip_prefix("commits/")
            && let Some(stem) = rest.strip_suffix(".json")
            && !stem.contains('/')
            && let Err(e) = Commit::parse_history(bytes)
        {
            return Err(format!("damaged commit {name}: {e}"));
        }
    }
    Ok(())
}

/// A name that can sit in `author {name} <email>` without becoming another field.
fn git_person(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| *c != '<' && *c != '>' && !c.is_control())
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        "Alto".into()
    } else {
        cleaned
    }
}
fn identity(label: &str, value: &str, email: bool) -> Result<()> {
    if value.is_empty()
        || value
            .chars()
            .any(|c| c.is_control() || c == '<' || c == '>')
        || (email && value.contains(' '))
    {
        return Err(format!("{label} must be a single line"));
    }
    Ok(())
}

/// A branch name that can be placed after `refs/heads/` without becoming another command.
fn check_branch_name(name: &str) -> Result<()> {
    let component =
        |part: &str| part.is_empty() || part.starts_with('.') || part.ends_with(".lock");
    if name.is_empty()
        || name == "@"
        || name.starts_with('/')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.contains("//")
        || name.contains("..")
        || name.contains("@{")
        || name.split('/').any(component)
        || name.chars().any(|c| {
            c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\' | '\u{7f}')
        })
    {
        return Err(format!("branch name is not a single git ref: {name}"));
    }
    Ok(())
}

/// Answers from `git cat-file --batch`, in request order. None is a missing object.
fn cat_batch(root: &Path, specs: &[String]) -> Result<Vec<Option<Vec<u8>>>> {
    let mut input = specs.join("\n").into_bytes();
    input.push(b'\n');
    let out = git_stdin(root, &["cat-file", "--batch"], &input)?;
    let mut rest = out.as_slice();
    let mut answers = Vec::with_capacity(specs.len());
    for _ in specs {
        let end = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("cat-file: truncated output")?;
        let header = String::from_utf8_lossy(&rest[..end]).into_owned();
        rest = &rest[end + 1..];
        if header.ends_with(" missing") {
            answers.push(None);
            continue;
        }
        let size: usize = header
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or("cat-file: bad header")?;
        let body = rest.get(..size).ok_or("cat-file: truncated body")?;
        answers.push(Some(body.to_vec()));
        rest = rest.get(size + 1..).ok_or("cat-file: truncated body")?;
    }
    Ok(answers)
}

/// The DAG reachable from every reference, parents first.
fn reachable(p: &Opened, roots: &[ContentHash]) -> Result<Vec<ContentHash>> {
    let mut order = vec![];
    let mut seen = BTreeSet::new();
    let mut stack: Vec<(ContentHash, bool)> = roots.iter().rev().map(|h| (*h, false)).collect();
    while let Some((h, expanded)) = stack.pop() {
        if expanded {
            order.push(h);
        } else if seen.insert(h) {
            let c = p
                .commits
                .get(&h)
                .ok_or_else(|| format!("CORRUPT_DATA: missing commit {h}"))?;
            stack.push((h, true));
            stack.extend(c.parents.iter().rev().map(|parent| (*parent, false)));
        }
    }
    Ok(order)
}

fn ancestors(p: &Opened, head: ContentHash) -> BTreeSet<ContentHash> {
    let mut keep = BTreeSet::new();
    let mut stack = vec![head];
    while let Some(h) = stack.pop() {
        if keep.insert(h)
            && let Some(c) = p.commits.get(&h)
        {
            stack.extend(c.parents.iter().copied());
        }
    }
    keep
}

/// Paths a commit adds to the package: itself, its snapshot and what the snapshot uses.
fn own_files(
    p: &Opened,
    h: &ContentHash,
    available: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let c = &p.commits[h];
    let snapshot_path = alto_store::snapshot_path(&c.snapshot);
    let snapshot = p
        .entries
        .get(&snapshot_path)
        .ok_or_else(|| format!("commit {} の snapshot がフォルダにありません", short(h)))?;
    let doc = Document::from_snapshot_bytes(snapshot).map_err(|e| e.to_string())?;
    let mut needed = BTreeSet::new();
    for n in doc.nodes.values() {
        if let NodeKind::Raster { tiles, .. } = &n.kind {
            needed.extend(tiles.tiles.values().copied());
        }
        if let Some(m) = &n.props.mask {
            needed.extend(m.tiles.values().copied());
        }
    }
    needed.extend(doc.assets.values().map(|a| a.hash));
    if let Some(missing) = needed
        .iter()
        .map(alto_store::object_path)
        .find(|o| !available.contains(o))
    {
        return Err(format!(
            "commit {} のデータがフォルダにありません: {missing}",
            short(h)
        ));
    }
    let mut files = referenced(snapshot, available);
    files.insert(snapshot_path);
    files.insert(alto_store::commit_path(h));
    Ok(files)
}

/// The manifest stored in a mirrored commit. A Git tree holds only that commit's
/// ancestry, so a checked-out tree stays a valid package: a branch tip names the
/// references inside its ancestry, and any other commit opens detached.
fn mirror_manifest(p: &Opened, h: &ContentHash, c: &Commit) -> Vec<u8> {
    let refs = &p.package.refs;
    let is_head = p.package.head == Some(*h);
    let tip_of = refs
        .branches
        .iter()
        .find(|(_, b)| b.head == *h)
        .map(|(name, _)| name.clone());
    if !is_head && tip_of.is_none() {
        return alto_store::manifest_bytes(c.snapshot, Some(*h));
    }
    let inside = ancestors(p, *h);
    let mut view = refs.clone();
    view.branches.retain(|_, b| inside.contains(&b.head));
    view.tags.retain(|_, t| inside.contains(t));
    view.pinned.retain(|t| inside.contains(t));
    if !is_head {
        view.current_branch = tip_of;
    }
    alto_store::manifest_bytes_with_refs(c.snapshot, Some(*h), view)
}

fn tree_files(root: &Path, commit: &str, prefix: &str) -> Result<BTreeSet<String>> {
    let mut args = vec!["ls-tree", "-r", "-z", "--name-only", commit];
    if !prefix.is_empty() {
        args.extend(["--", prefix]);
    }
    let out = git_bytes(root, &args)?;
    Ok(out
        .split(|b| *b == 0)
        .filter_map(|name| std::str::from_utf8(name).ok())
        .filter_map(|name| name.strip_prefix(prefix).map(str::to_string))
        .collect())
}

/// Checks the recorded table against the Git objects it names: the trailer, the
/// parents in order, and the commit file inside the tree.
fn verify_table(p: &Opened, root: &Path, state: &SyncState, order: &[ContentHash]) -> Result<()> {
    // Only reachable versions matter. Git may prune the commit of a deleted branch.
    let table: Vec<(&ContentHash, &String)> = order
        .iter()
        .filter_map(|h| Some((h, state.commits.get(h)?)))
        .collect();
    let mut specs = Vec::with_capacity(table.len() * 2);
    for (h, oid) in &table {
        specs.push(format!("{oid}^{{commit}}"));
        specs.push(format!(
            "{oid}:{}{}",
            state.prefix,
            alto_store::commit_path(h)
        ));
    }
    if specs.is_empty() {
        return Ok(());
    }
    let answers = cat_batch(root, &specs)?;
    for (i, (h, oid)) in table.iter().enumerate() {
        let broken = |why: &str| conflict(format!("mapping {} -> {oid}: {why}", short(h)));
        let raw = answers[i * 2]
            .as_ref()
            .ok_or_else(|| broken("Git commit is missing"))?;
        let text = String::from_utf8_lossy(raw);
        let (headers, body) = text
            .split_once("\n\n")
            .ok_or_else(|| broken("unreadable Git commit"))?;
        let trailer = body
            .lines()
            .rev()
            .find_map(|l| l.strip_prefix("Alto-Commit: "));
        if trailer != Some(h.hex().as_str()) {
            return Err(broken("Alto-Commit trailer does not match"));
        }
        let parents: Vec<&str> = headers
            .lines()
            .filter_map(|l| l.strip_prefix("parent "))
            .collect();
        let alto_parents = &p.commits[h].parents;
        let expected: Vec<&str> = if alto_parents.is_empty() {
            state.base.iter().map(String::as_str).collect()
        } else {
            alto_parents
                .iter()
                .map(|parent| {
                    state
                        .commits
                        .get(parent)
                        .map(String::as_str)
                        .ok_or_else(|| broken("a parent is not mapped"))
                })
                .collect::<Result<_>>()?
        };
        if parents != expected {
            return Err(broken("Git parents differ from the Alto parents"));
        }
        let file = answers[i * 2 + 1]
            .as_ref()
            .ok_or_else(|| broken("commit file is not in the Git tree"))?;
        if ContentHash::of(file) != **h {
            return Err(broken("commit file in the Git tree has other content"));
        }
    }
    Ok(())
}

fn load_state(root: &Path, state_ref: &str) -> Result<Option<(String, SyncState)>> {
    let Some(oid) = rev_parse(root, state_ref) else {
        return Ok(None);
    };
    let bytes = git_bytes(root, &["cat-file", "blob", &oid])
        .map_err(|e| conflict(format!("{state_ref}: {e}")))?;
    let state: SyncState =
        serde_json::from_slice(&bytes).map_err(|e| conflict(format!("{state_ref}: {e}")))?;
    if state.format != STATE_FORMAT {
        return Err(format!("UNSUPPORTED: {state_ref} format {}", state.format));
    }
    Ok(Some((oid, state)))
}

/// Refs that another package in this repository already maps.
fn foreign_refs(root: &Path, own_state: &str) -> Result<BTreeMap<String, String>> {
    let listed = git(
        root,
        &["for-each-ref", "--format=%(refname)", "refs/alto/*/state"],
    )?;
    let mut refs = BTreeMap::new();
    for name in listed.lines().filter(|l| *l != own_state) {
        if let Some((_, state)) = load_state(root, name)? {
            refs.insert(state.trunk_ref.clone(), state.document.clone());
            for e in state.branches.values().chain(state.tags.values()) {
                refs.insert(e.git_ref.clone(), state.document.clone());
            }
        }
    }
    Ok(refs)
}

/// A change to one mapped ref, decided before any Git object is written.
struct Planned {
    git_ref: String,
    alto: ContentHash,
    old: Option<String>,
}

struct RefCheck<'a> {
    label: &'a str,
    name: &'a str,
    head: ContentHash,
    entry: Option<&'a RefEntry>,
    git_ref: &'a str,
    allow_existing: bool,
    fast_forward_only: bool,
}

/// Applies the branch rules: a mapped ref must still hold the recorded commit and an
/// Alto branch may only move forward. A new ref must not exist yet.
fn check_mapped(p: &Opened, root: &Path, check: RefCheck) -> Result<Planned> {
    let RefCheck {
        label,
        name,
        head,
        entry,
        git_ref,
        allow_existing,
        fast_forward_only,
    } = check;
    let current = rev_parse(root, git_ref);
    let old = match entry {
        Some(e) => {
            if current.as_deref() != Some(e.git.as_str()) {
                return Err(conflict(format!(
                    "{git_ref} was changed outside alto-git: expected {}, found {}",
                    e.git,
                    current.as_deref().unwrap_or("no ref")
                )));
            }
            if fast_forward_only && !ancestors(p, head).contains(&e.alto) {
                return Err(conflict(format!(
                    "Alto {label} {name} no longer descends from the commit mapped to {git_ref}"
                )));
            }
            current
        }
        None => {
            if current.is_some() && !allow_existing {
                return Err(conflict(format!(
                    "{git_ref} already exists and is not mapped to Alto {label} {name}"
                )));
            }
            current
        }
    };
    Ok(Planned {
        git_ref: git_ref.to_string(),
        alto: head,
        old,
    })
}

/// Sync without options, the form the command line uses for `--branch` alone.
pub fn sync(folder_dir: &Path, branch: Option<&str>) -> Result<SyncReport> {
    sync_with(
        folder_dir,
        SyncOptions {
            branch,
            ref_prefix: None,
        },
    )
}

/// Git Adapter: mirrors every commit reachable from the Alto references, parents first.
/// Each Git commit's tree is the folder as it was at that Alto commit; objects are only
/// ever added, so later commits reuse earlier blobs.
pub fn sync_with(folder_dir: &Path, options: SyncOptions) -> Result<SyncReport> {
    if let Some(name) = options.branch {
        check_branch_name(name)?;
    }
    if let Some(name) = options.ref_prefix {
        check_branch_name(name)?;
    }
    let p = Opened::from_entries(folder::read(folder_dir).map_err(|e| e.to_string())?)?;
    reject_damaged_commits(&p.entries)?;
    // A commit that does not parse is not imported. Angle brackets in a stored
    // author are a warning, and the fast-import line uses `git_person`.
    let prefix = git(folder_dir, &["rev-parse", "--show-prefix"])?
        .trim()
        .to_string();
    let root = PathBuf::from(git(folder_dir, &["rev-parse", "--show-toplevel"])?.trim());
    let current = git(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string());
    path_token("path", &prefix)?;

    let document = p.doc.root.0.to_string();
    let namespace = format!("refs/alto/{document}");
    let state_ref = format!("{namespace}/state");
    let staging = format!("{namespace}/staging");
    let loaded = load_state(&root, &state_ref)?;
    let refs = &p.package.refs;
    let trunk_alto = match &loaded {
        Some((_, s)) => s.trunk_alto.clone(),
        None => refs
            .current_branch
            .clone()
            .or_else(|| refs.branches.contains_key("main").then(|| "main".into()))
            .or_else(|| refs.branches.keys().next().cloned()),
    };
    let requested = options
        .branch
        .map(str::to_string)
        .or_else(|| current.clone())
        .unwrap_or_else(|| "main".into());
    check_branch_name(&requested)?;
    let trunk_ref = match &loaded {
        Some((_, s)) => {
            if options
                .branch
                .is_some_and(|b| format!("refs/heads/{b}") != s.trunk_ref)
            {
                return Err(conflict(format!(
                    "this package is already mapped to {}",
                    s.trunk_ref
                )));
            }
            s.trunk_ref.clone()
        }
        None => format!("refs/heads/{requested}"),
    };
    let branch_label = trunk_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&trunk_ref)
        .to_string();
    let ref_prefix = match &loaded {
        Some((_, s)) => {
            if options.ref_prefix.is_some_and(|r| r != s.ref_prefix) {
                return Err(conflict(format!(
                    "this package already uses the ref prefix {}",
                    s.ref_prefix
                )));
            }
            s.ref_prefix.clone()
        }
        None => options.ref_prefix.unwrap_or(DEFAULT_REF_PREFIX).to_string(),
    };
    if let Some((_, s)) = &loaded
        && (s.document != document || s.prefix != prefix)
    {
        return Err(conflict(format!(
            "{state_ref} was recorded for {} at {:?}",
            s.document, s.prefix
        )));
    }

    let mut roots: Vec<ContentHash> = vec![];
    if let Some(trunk) = trunk_alto.as_ref().and_then(|t| refs.branches.get(t)) {
        roots.push(trunk.head);
    }
    roots.extend(refs.roots());
    roots.extend(p.package.head);
    let order = reachable(&p, &roots)?;
    if order.is_empty() {
        return Ok(SyncReport {
            created: vec![],
            branch: branch_label,
            refs: vec![],
            warnings: vec![],
        });
    }

    let base = match &loaded {
        Some((_, s)) => s.base.clone(),
        None => rev_parse(&root, &trunk_ref),
    };
    let empty = BTreeMap::new();
    let old_state = loaded.as_ref().map(|(_, s)| s);
    let old_branches = old_state.map_or(&empty, |s| &s.branches);
    let old_tags = old_state.map_or(&empty, |s| &s.tags);
    match old_state {
        Some(s) => {
            if let Some(b) = &s.base {
                git(&root, &["cat-file", "-e", &format!("{b}^{{commit}}")]).map_err(conflict)?;
            }
            verify_table(&p, &root, s, &order)?;
        }
        None => {
            // Without the table the same versions would be imported a second time.
            let seen = git(
                &root,
                &[
                    "log",
                    "--exclude=refs/alto/*/staging",
                    "--all",
                    "--format=%(trailers:key=Alto-Commit,valueonly)",
                ],
            )?;
            if let Some(h) = seen
                .lines()
                .find_map(|l| l.trim().parse::<ContentHash>().ok())
                .filter(|h| p.commits.contains_key(h))
            {
                return Err(conflict(format!(
                    "commit {} is already in this repository but {state_ref} is missing",
                    short(&h)
                )));
            }
        }
    }

    // Decide every ref change first. A rejection here has written nothing.
    let foreign = foreign_refs(&root, &state_ref)?;
    let not_foreign = |git_ref: &str| match foreign.get(git_ref) {
        Some(owner) => Err(conflict(format!(
            "{git_ref} is mapped by document {owner}; choose --branch or --ref-prefix"
        ))),
        None => Ok(()),
    };
    let mut warnings = vec![];
    let mut planned_branches = BTreeMap::new();
    for (name, branch) in &refs.branches {
        let entry = old_branches.get(name);
        let git_ref = match entry {
            Some(e) => e.git_ref.clone(),
            None if trunk_alto.as_deref() == Some(name) => trunk_ref.clone(),
            None => {
                let short_ref = format!("{ref_prefix}/{name}");
                check_branch_name(&short_ref)?;
                format!("refs/heads/{short_ref}")
            }
        };
        not_foreign(&git_ref)?;
        let first_trunk = old_state.is_none() && git_ref == trunk_ref;
        planned_branches.insert(
            name.clone(),
            check_mapped(
                &p,
                &root,
                RefCheck {
                    label: "branch",
                    name,
                    head: branch.head,
                    entry,
                    git_ref: &git_ref,
                    allow_existing: first_trunk,
                    fast_forward_only: true,
                },
            )?,
        );
    }
    let mut planned_tags = BTreeMap::new();
    for (name, head) in &refs.tags {
        let entry = old_tags.get(name);
        let git_ref = match entry {
            Some(e) => e.git_ref.clone(),
            None => {
                let short_ref = format!("{ref_prefix}/{name}");
                check_branch_name(&short_ref)?;
                format!("refs/tags/{short_ref}")
            }
        };
        not_foreign(&git_ref)?;
        planned_tags.insert(
            name.clone(),
            check_mapped(
                &p,
                &root,
                RefCheck {
                    label: "tag",
                    name,
                    head: *head,
                    entry,
                    git_ref: &git_ref,
                    allow_existing: false,
                    fast_forward_only: false,
                },
            )?,
        );
    }
    // Refs of case-insensitive file systems share one file, so `Foo` and `foo` collide.
    let mut folded = BTreeMap::new();
    for plan in planned_branches.values().chain(planned_tags.values()) {
        if let Some(other) = folded.insert(plan.git_ref.to_lowercase(), plan.git_ref.clone()) {
            return Err(format!(
                "UNSUPPORTED: {other} and {} are the same Git ref when case is ignored",
                plan.git_ref
            ));
        }
    }
    let mut deletions: Vec<(String, String)> = vec![];
    for (kind, old, kept) in [
        (
            "branch",
            old_branches,
            &refs.branches.keys().collect::<BTreeSet<_>>(),
        ),
        ("tag", old_tags, &refs.tags.keys().collect::<BTreeSet<_>>()),
    ] {
        for (name, e) in old {
            if kept.contains(name) {
                continue;
            }
            match rev_parse(&root, &e.git_ref) {
                None => {}
                Some(found) if found != e.git => warnings.push(format!(
                    "{} was changed outside alto-git and stays after Alto {kind} {name} was removed",
                    e.git_ref
                )),
                Some(_) if !e.created => warnings.push(format!(
                    "{} existed before the first sync and stays after Alto {kind} {name} was removed",
                    e.git_ref
                )),
                Some(found) => deletions.push((e.git_ref.clone(), found)),
            }
        }
    }

    // A version that left the package after Alto GC leaves the table too.
    let mut table: BTreeMap<ContentHash, String> = old_state
        .into_iter()
        .flat_map(|s| &s.commits)
        .filter(|(h, _)| p.commits.contains_key(h))
        .map(|(h, oid)| (*h, oid.clone()))
        .collect();
    let todo: Vec<ContentHash> = order
        .iter()
        .filter(|h| !table.contains_key(h))
        .copied()
        .collect();
    let mut created = vec![];
    if !todo.is_empty() {
        let name = git(&root, &["config", "user.name"])
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "Alto".into());
        let email = git(&root, &["config", "user.email"])
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "alto@localhost".into());
        identity("committer", &name, false)?;
        identity("email", &email, true)?;
        let (stream, made) = commit_stream(
            &p,
            &StreamContext {
                root: &root,
                prefix: &prefix,
                staging: &staging,
                table: &table,
                base: base.as_deref(),
                name: &name,
                email: &email,
            },
            &todo,
        )?;
        created = made;
        let marks = run_fast_import(&root, &stream, &staging)?;
        for (n, h) in todo.iter().enumerate() {
            let oid = marks
                .get(&(n + 1))
                .ok_or("git fast-import did not report a commit")?;
            table.insert(*h, oid.clone());
        }
    }

    let entry_for = |plan: &Planned, previous: Option<&RefEntry>| RefEntry {
        git_ref: plan.git_ref.clone(),
        alto: plan.alto,
        git: table[&plan.alto].clone(),
        created: previous.map_or(plan.old.is_none(), |e| e.created),
    };
    let new_branches: BTreeMap<String, RefEntry> = planned_branches
        .iter()
        .map(|(n, plan)| (n.clone(), entry_for(plan, old_branches.get(n))))
        .collect();
    let new_tags: BTreeMap<String, RefEntry> = planned_tags
        .iter()
        .map(|(n, plan)| (n.clone(), entry_for(plan, old_tags.get(n))))
        .collect();
    // A pin or a detached head may sit outside every branch and tag.
    let detached = p.package.head.filter(|_| refs.current_branch.is_none());
    let keep: BTreeMap<ContentHash, String> = refs
        .pinned
        .iter()
        .copied()
        .chain(detached)
        .map(|h| (h, table[&h].clone()))
        .collect();
    let state = SyncState {
        format: STATE_FORMAT,
        document,
        prefix: prefix.clone(),
        ref_prefix,
        base,
        trunk_alto,
        trunk_ref: trunk_ref.clone(),
        commits: table.clone(),
        branches: new_branches,
        tags: new_tags,
        keep,
    };

    let mut tx = String::new();
    let mut updates = vec![];
    for plan in planned_branches.values().chain(planned_tags.values()) {
        let new = &table[&plan.alto];
        match &plan.old {
            Some(old) if old == new => {}
            Some(old) => {
                tx += &format!("update {} {new} {old}\n", plan.git_ref);
                updates.push(RefUpdate {
                    git_ref: plan.git_ref.clone(),
                    commit: Some(new.clone()),
                });
            }
            None => {
                tx += &format!("create {} {new}\n", plan.git_ref);
                updates.push(RefUpdate {
                    git_ref: plan.git_ref.clone(),
                    commit: Some(new.clone()),
                });
            }
        }
    }
    for (git_ref, old) in &deletions {
        tx += &format!("delete {git_ref} {old}\n");
        updates.push(RefUpdate {
            git_ref: git_ref.clone(),
            commit: None,
        });
    }
    let old_keep = old_state.map(|s| &s.keep);
    for (h, oid) in &state.keep {
        if old_keep.and_then(|k| k.get(h)) != Some(oid) {
            tx += &format!("update {namespace}/keep/{h} {oid}\n");
        }
    }
    for h in old_keep.into_iter().flatten().map(|(h, _)| h) {
        if !state.keep.contains_key(h) {
            tx += &format!("delete {namespace}/keep/{h}\n");
        }
    }
    if !todo.is_empty() {
        tx += &format!("delete {staging}\n");
    }
    if old_state != Some(&state) {
        let mut bytes = serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        let blob = git_stdin(&root, &["hash-object", "-w", "--stdin"], &bytes)?;
        let blob = String::from_utf8_lossy(&blob).trim().to_string();
        match &loaded {
            Some((old, _)) => tx += &format!("update {state_ref} {blob} {old}\n"),
            None => tx += &format!("create {state_ref} {blob}\n"),
        }
    }
    if !tx.is_empty()
        && let Err(e) = git_stdin(&root, &["update-ref", "--stdin"], tx.as_bytes())
    {
        let _ = git(&root, &["update-ref", "-d", &staging]);
        return Err(conflict(e));
    }
    // The checked-out branch moved under the index: re-read only the package folder.
    if let Some(checked_out) = &current {
        let checked_out = format!("refs/heads/{checked_out}");
        if updates
            .iter()
            .any(|u| u.git_ref == checked_out && u.commit.is_some())
        {
            let path = if prefix.is_empty() {
                "."
            } else {
                prefix.as_str()
            };
            git(&root, &["reset", "-q", "--", path])?;
        }
    }
    Ok(SyncReport {
        created,
        branch: branch_label,
        refs: updates,
        warnings,
    })
}

struct StreamContext<'a> {
    root: &'a Path,
    prefix: &'a str,
    staging: &'a str,
    table: &'a BTreeMap<ContentHash, String>,
    base: Option<&'a str>,
    name: &'a str,
    email: &'a str,
}

type Created = Vec<(ContentHash, String)>;

/// Builds the fast-import stream for `todo`, which is ordered parents first.
fn commit_stream(
    p: &Opened,
    context: &StreamContext,
    todo: &[ContentHash],
) -> Result<(Vec<u8>, Created)> {
    let StreamContext {
        root,
        prefix,
        staging,
        table,
        base,
        name,
        email,
    } = *context;
    let available: BTreeSet<String> = p
        .entries
        .keys()
        .filter(|k| k.starts_with("objects/"))
        .cloned()
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let marks: BTreeMap<ContentHash, usize> =
        todo.iter().enumerate().map(|(n, h)| (*h, n + 1)).collect();
    let from_spec = |parent: &ContentHash| -> Result<String> {
        match (marks.get(parent), table.get(parent)) {
            (Some(mark), _) => Ok(format!(":{mark}")),
            (None, Some(oid)) => Ok(oid.clone()),
            (None, None) => Err(format!("commit {} has no Git commit", short(parent))),
        }
    };
    // The files in each mirrored tree, read once from Git for a parent mirrored earlier.
    let mut trees: BTreeMap<ContentHash, BTreeSet<String>> = BTreeMap::new();
    let mut stream: Vec<u8> = vec![];
    let mut created = vec![];
    for (n, h) in todo.iter().enumerate() {
        let c = &p.commits[h];
        let snap_bytes = p
            .entries
            .get(&alto_store::snapshot_path(&c.snapshot))
            .cloned()
            .ok_or_else(|| format!("commit {} の snapshot がフォルダにありません", short(h)))?;
        let doc = Document::from_snapshot_bytes(&snap_bytes).map_err(|e| e.to_string())?;
        let mut message = c.message.trim().to_string();
        if message.is_empty() {
            message = "（メッセージなし）".into();
        }
        let intents = intent_lines(&doc, c);
        if !intents.is_empty() {
            message += &format!("\n\n{}", intents.join("\n"));
        }
        let parent_doc = match c.parents.first() {
            None => Some(crate::empty_like(&doc)),
            Some(ph) => p
                .commits
                .get(ph)
                .and_then(|pc| p.snapshot_doc(&pc.snapshot).ok()),
        };
        if let Some(parent) = parent_doc {
            let lines = crate::render_diff(&parent, &doc, &alto_doc::diff(&parent, &doc));
            if !lines.is_empty() {
                message += &format!(
                    "\n\n{}",
                    lines.into_iter().take(200).collect::<Vec<_>>().join("\n")
                );
            }
        }
        message += &format!("\n\nAlto-Commit: {h}\nAlto-Snapshot: {}\n", c.snapshot);
        if let Some(merge_base) = &c.merge_base {
            message += &format!("Alto-Merge-Base: {merge_base}\n");
        }
        let who = git_person(&c.author.clone().unwrap_or_else(|| name.to_string()));
        identity("author", &who, false)?;
        // `reset` keeps fast-import from taking the previous commit as an implicit parent.
        let _ = writeln!(stream, "reset {staging}");
        let _ = write!(stream, "commit {staging}\nmark :{}\n", n + 1);
        let _ = writeln!(
            stream,
            "author {who} <{email}> {} +0000",
            c.created_at.div_euclid(1000)
        );
        let _ = writeln!(stream, "committer {name} <{email}> {now} +0000");
        let _ = write!(stream, "data {}\n{message}\n", message.len());
        let mut present = match c.parents.first() {
            Some(first) => {
                let _ = writeln!(stream, "from {}", from_spec(first)?);
                match trees.get(first) {
                    Some(files) => files.clone(),
                    None => tree_files(root, &from_spec(first)?, prefix)?,
                }
            }
            None => match base {
                Some(tip) => {
                    let _ = writeln!(stream, "from {tip}");
                    tree_files(root, tip, prefix)?
                }
                None => BTreeSet::new(),
            },
        };
        let mut wanted = own_files(p, h, &available)?;
        if let Some(second) = c.parents.get(1) {
            let _ = writeln!(stream, "merge {}", from_spec(second)?);
            // The tree starts from the first parent, so the second side's history is
            // added here. A checkout of the merge has the whole DAG.
            let ours = ancestors(p, c.parents[0]);
            for theirs in ancestors(p, *second).difference(&ours) {
                wanted.extend(own_files(p, theirs, &available)?);
            }
        }
        let mut files: Vec<(String, Vec<u8>)> = vec![];
        for path in wanted {
            let data = p
                .entries
                .get(&path)
                .ok_or_else(|| format!("{path} がフォルダにありません"))?;
            files.push((path, data.clone()));
        }
        files.push(("manifest.json".into(), mirror_manifest(p, h, c)));
        files.push(("document.json".into(), snap_bytes));
        files.push((
            ".gitattributes".into(),
            alto_store::GITATTRIBUTES.as_bytes().to_vec(),
        ));
        files.push((
            ".gitignore".into(),
            b".alto.lock\n.alto-gc.json\n.*.tmp\n".to_vec(),
        ));
        for (path, data) in files {
            if alto_store::is_immutable(&path) && present.contains(&path) {
                continue;
            }
            path_token("path", &path)?;
            if alto_store::is_immutable(&path) && !alto_store::bytes_match_name(&path, &data) {
                return Err(format!("content does not match {path}"));
            }
            let _ = write!(
                stream,
                "M 100644 inline {prefix}{path}\ndata {}\n",
                data.len()
            );
            stream.extend_from_slice(&data);
            stream.push(b'\n');
            present.insert(path);
        }
        stream.push(b'\n');
        trees.insert(*h, present);
        created.push((*h, c.message.lines().next().unwrap_or("").to_string()));
    }
    stream.extend_from_slice(b"done\n");
    Ok((stream, created))
}

/// Imports under the private staging ref and returns mark numbers mapped to Git
/// commits. No visible ref moves until the caller's transaction.
fn run_fast_import(root: &Path, stream: &[u8], staging: &str) -> Result<BTreeMap<usize, String>> {
    // A sync that stopped before its transaction leaves only this ref behind.
    let _ = git(root, &["update-ref", "-d", staging]);
    let git_dir = PathBuf::from(git(root, &["rev-parse", "--absolute-git-dir"])?.trim());
    let marks_path = git_dir.join(format!("alto-marks-{}", std::process::id()));
    let marks_arg = format!("--export-marks={}", marks_path.display());
    let result = git_stdin(
        root,
        &["fast-import", "--quiet", "--done", &marks_arg],
        stream,
    );
    let marks = std::fs::read_to_string(&marks_path);
    let _ = std::fs::remove_file(&marks_path);
    result.map_err(|e| format!("git fast-import: {e}"))?;
    let marks = marks.map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    for line in marks.lines() {
        let (mark, oid) = line.split_once(' ').ok_or("git fast-import marks")?;
        let mark: usize = mark
            .trim_start_matches(':')
            .parse()
            .map_err(|_| "git fast-import marks")?;
        out.insert(mark, oid.to_string());
    }
    Ok(out)
}
