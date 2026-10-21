//! Git Adapter for Alto folder packages (`.altoproj/`).
//!
//! The engine never links Git. This crate reads the folder form, mirrors each Alto
//! commit into one Git commit with `git fast-import`, and renders the semantic diff
//! layer by layer for terminals and `git diff` (textconv). Native only.
#![cfg(not(target_arch = "wasm32"))]
use alto_core::ContentHash;
use alto_doc::{Commit, DiffItem, Document, NodeKind};
use alto_store::{Package, folder};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub type Result<T> = std::result::Result<T, String>;

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git を実行できません: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(out.stdout)
}

/// A readable package: the decoded parts plus its raw entries.
pub struct Opened {
    pub package: Package,
    pub entries: BTreeMap<String, Vec<u8>>,
    pub doc: Document,
    pub commits: BTreeMap<ContentHash, Commit>,
    /// Commits whose bytes match their name but fail `Commit::parse`.
    /// Opening still returns the document. `sync` refuses the folder.
    pub warnings: Vec<String>,
}
impl Opened {
    pub fn from_entries(entries: BTreeMap<String, Vec<u8>>) -> Result<Self> {
        let package = alto_store::decode_entries(entries.clone()).map_err(|e| e.to_string())?;
        let doc = Document::from_snapshot_bytes(&package.snapshot).map_err(|e| e.to_string())?;
        let mut commits = BTreeMap::new();
        let mut warnings = Vec::new();
        for (h, b) in &package.commits {
            match Commit::parse(b) {
                Ok(c) => {
                    commits.insert(*h, c);
                }
                Err(e) => warnings.push(format!("damaged commit {h}: {e}")),
            }
        }
        Ok(Self {
            package,
            entries,
            doc,
            commits,
            warnings,
        })
    }
    /// Opens a `.alto` file, a folder package, or `<git-rev>:<folder>` inside a repository.
    pub fn open(spec: &str) -> Result<Self> {
        let path = Path::new(spec);
        if path.is_dir() {
            return Self::from_entries(folder::read(path).map_err(|e| e.to_string())?);
        }
        if path.is_file() {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            return Self::from_entries(
                alto_store::read_entries(&bytes).map_err(|e| e.to_string())?,
            );
        }
        if let Some((rev, dir)) = spec.split_once(':') {
            return Self::from_git(Path::new(if dir.is_empty() { "." } else { dir }), rev);
        }
        Err(format!(
            "{spec}: .alto ファイル、フォルダ、または <rev>:<folder> を指定してください"
        ))
    }
    /// Reads a folder package as it was at a Git revision (`git show`).
    pub fn from_git(dir: &Path, rev: &str) -> Result<Self> {
        let prefix = git(dir, &["rev-parse", "--show-prefix"])?
            .trim()
            .to_string();
        let show = |name: &str| git_bytes(dir, &["show", &format!("{rev}:{prefix}{name}")]);
        let manifest = show("manifest.json")?;
        let m: alto_store::Manifest =
            serde_json::from_slice(&manifest).map_err(|e| e.to_string())?;
        let mut entries = BTreeMap::new();
        entries.insert("manifest.json".to_string(), manifest);
        let snap = alto_store::snapshot_path(&m.snapshot);
        entries.insert(snap.clone(), show(&snap)?);
        // History: commits and their snapshots; objects are not needed for diffs.
        let mut next = m.head;
        while let Some(h) = next {
            let path = alto_store::commit_path(&h);
            let Ok(bytes) = show(&path) else { break };
            let c = Commit::parse(&bytes)?;
            if let Ok(s) = show(&alto_store::snapshot_path(&c.snapshot)) {
                entries.insert(alto_store::snapshot_path(&c.snapshot), s);
            }
            entries.insert(path, bytes);
            next = c.parents.first().copied();
        }
        Self::from_entries(entries)
    }
    /// First-parent chain from the head, oldest first.
    pub fn chain(&self) -> Vec<ContentHash> {
        let mut out = vec![];
        let mut next = self.package.head;
        while let Some(h) = next {
            if out.contains(&h) {
                break;
            }
            let Some(c) = self.commits.get(&h) else { break };
            out.push(h);
            next = c.parents.first().copied();
        }
        out.reverse();
        out
    }
    pub fn snapshot_doc(&self, snapshot: &ContentHash) -> Result<Document> {
        if *snapshot == ContentHash::of(&self.package.snapshot) {
            return Ok(self.doc.clone());
        }
        let b = self
            .package
            .snapshots
            .get(snapshot)
            .ok_or_else(|| format!("snapshot {snapshot} がありません"))?;
        Document::from_snapshot_bytes(b).map_err(|e| e.to_string())
    }
}

/// UUIDv7 starts with a timestamp, so the random tail identifies nodes better.
fn tail(id: &str) -> &str {
    &id[id.len().saturating_sub(8)..]
}
fn short(h: &ContentHash) -> String {
    h.hex()[..10].to_string()
}
fn node_label(doc: &Document, id: &alto_core::NodeId) -> String {
    match doc.nodes.get(id) {
        Some(_) if *id == doc.root => "ルート".into(),
        Some(n) => format!("「{}」", n.name),
        None => "（不明）".into(),
    }
}

/// Human-readable, layer-level lines for a semantic diff.
pub fn render_diff(a: &Document, b: &Document, items: &[DiffItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            DiffItem::Added {
                name, kind, parent, ..
            } => {
                format!("+ 追加    [{kind}] {name}（{}内）", node_label(b, parent))
            }
            DiffItem::Removed { name, kind, .. } => format!("- 削除    [{kind}] {name}"),
            DiffItem::Moved { name, from, to, .. } => format!(
                "→ 移動    {name}: {}[{}] → {}[{}]",
                node_label(a, &from.0),
                from.1,
                node_label(b, &to.0),
                to.1
            ),
            DiffItem::PropsChanged { name, fields, .. } => {
                format!("~ 属性    {name}: {}", fields.join(", "))
            }
            DiffItem::PixelsChanged {
                name,
                target,
                tiles,
                document_bounds: r,
                ..
            } => format!(
                "~ 画素    {name}{}: {tiles}タイル 文書座標 ({},{})-({},{})",
                if target == "mask" {
                    "（マスク）"
                } else {
                    ""
                },
                r.x0,
                r.y0,
                r.x1,
                r.y1
            ),
            DiffItem::ContentChanged {
                name, kind, detail, ..
            } => {
                format!("~ 内容    [{kind}] {name}: {detail}")
            }
            DiffItem::AssetChanged { id, change, kind } => {
                let what = match change.as_str() {
                    "added" => "追加",
                    "removed" => "削除",
                    _ => "変更",
                };
                format!("~ アセット {kind} {what} {}", tail(&id.0.to_string()))
            }
            DiffItem::MetaChanged { fields } => format!("~ 文書    {}", fields.join(", ")),
            DiffItem::SelectionChanged { tiles } => format!("~ 選択範囲 {tiles}タイル"),
        })
        .collect()
}

pub fn diff_text(a: &Document, b: &Document) -> String {
    let items = alto_doc::diff(a, b);
    if items.is_empty() {
        return "変更なし\n".into();
    }
    let mut s = render_diff(a, b, &items).join("\n");
    s.push('\n');
    s
}

fn intent_lines(doc: &Document, c: &Commit) -> Vec<String> {
    c.intents
        .iter()
        .map(|i| {
            let targets: Vec<_> = i
                .targets
                .iter()
                .filter_map(|t| doc.nodes.get(t).map(|n| n.name.clone()))
                .collect();
            let count = if i.count > 1 {
                format!(" ×{}", i.count)
            } else {
                String::new()
            };
            if targets.is_empty() {
                format!("- {}{count}", i.intent)
            } else {
                format!("- {}{count}（{}）", i.intent, targets.join("、"))
            }
        })
        .collect()
}

/// The same document lineage with nothing in it, for diffs of a first commit.
pub fn empty_like(doc: &Document) -> Document {
    let mut e = doc.clone();
    e.nodes.retain(|k, _| *k == doc.root);
    if let Some(NodeKind::Group { children, .. }) = e.nodes.get_mut(&doc.root).map(|n| &mut n.kind)
    {
        children.clear();
    }
    e.assets.clear();
    e.selection = None;
    e
}

/// `alto-git log`: Alto commits with their intents and per-layer changes.
pub fn log_text(p: &Opened) -> Result<String> {
    let mut s = String::new();
    let chain = p.chain();
    for (i, h) in chain.iter().enumerate().rev() {
        let c = &p.commits[h];
        let doc = p.snapshot_doc(&c.snapshot)?;
        s += &format!(
            "commit {}  {}\n",
            short(h),
            c.message.lines().next().unwrap_or("")
        );
        if let Some(a) = &c.author {
            s += &format!("作成者 {a}\n");
        }
        for l in intent_lines(&doc, c) {
            s += &format!("  {l}\n");
        }
        let parent = if i == 0 {
            empty_like(&doc)
        } else {
            p.snapshot_doc(&p.commits[&chain[i - 1]].snapshot)?
        };
        for l in render_diff(&parent, &doc, &alto_doc::diff(&parent, &doc)) {
            s += &format!("    {l}\n");
        }
        s.push('\n');
    }
    if chain.is_empty() {
        s += "コミットはまだありません\n";
    }
    Ok(s)
}

/// `textconv` for document.json: one header per layer, one line per tile, so that
/// plain `git diff` hunks read layer by layer.
pub fn textconv(snapshot: &[u8]) -> Result<String> {
    let doc = Document::from_snapshot_bytes(snapshot).map_err(|e| e.to_string())?;
    let mut s = format!(
        "文書 {}×{} {:?} {:?}\n",
        doc.meta.width, doc.meta.height, doc.meta.color, doc.meta.depth
    );
    fn walk(doc: &Document, id: alto_core::NodeId, depth: usize, s: &mut String) {
        let Some(n) = doc.nodes.get(&id) else { return };
        let pad = "  ".repeat(depth);
        let p = &n.props;
        let t = p.transform.0;
        *s += &format!(
            "{pad}[{}] {}  id={} visible={} opacity={} blend={:?} transform=[{} {} {} {} {} {}]{}{}{}\n",
            n.kind_name(),
            n.name,
            tail(&n.id.0.to_string()),
            p.visible,
            p.opacity,
            p.blend,
            t[0],
            t[1],
            t[2],
            t[3],
            t[4],
            t[5],
            if p.clip_to_below { " clip" } else { "" },
            if p.mask.is_some() { " mask" } else { "" },
            if p.effects.is_empty() {
                String::new()
            } else {
                format!(" effects={}", p.effects.len())
            },
        );
        match &n.kind {
            NodeKind::Raster { tiles } => {
                for (k, h) in &tiles.tiles {
                    *s += &format!("{pad}  tile {k} {}\n", short(h));
                }
            }
            NodeKind::Vector { paths } => {
                for (i, p) in paths.iter().enumerate() {
                    *s += &format!("{pad}  path {i}: {}\n", p.data.to_svg());
                }
            }
            NodeKind::Text { content } => {
                *s += &format!(
                    "{pad}  {:?} size={} fonts={}\n",
                    content.writing_mode,
                    content.style.size,
                    content.style.fonts.len()
                );
                for line in content.text.lines() {
                    *s += &format!("{pad}  | {line}\n");
                }
            }
            NodeKind::Adjustment { adjustment } => *s += &format!("{pad}  {adjustment}\n"),
            _ => {}
        }
        if let Some(m) = &p.mask {
            for (k, h) in &m.tiles {
                *s += &format!("{pad}  mask {k} {}\n", short(h));
            }
        }
        if let Some(children) = n.children() {
            for c in children {
                walk(doc, *c, depth + 1, s);
            }
        }
    }
    walk(&doc, doc.root, 0, &mut s);
    Ok(s)
}

/// Object hashes referenced by a snapshot (any 64-hex value naming an existing object).
fn referenced(snapshot: &[u8], available: &BTreeSet<String>) -> BTreeSet<String> {
    fn walk(v: &serde_json::Value, out: &mut BTreeSet<String>) {
        match v {
            serde_json::Value::String(s)
                if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                out.insert(s.clone());
            }
            serde_json::Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            serde_json::Value::Object(o) => o.values().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut hashes = BTreeSet::new();
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(snapshot) {
        walk(&v, &mut hashes);
    }
    hashes
        .into_iter()
        .filter_map(|h| h.parse::<ContentHash>().ok())
        .map(|h| alto_store::object_path(&h))
        .filter(|p| available.contains(p))
        .collect()
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
            && let Err(e) = Commit::parse(bytes)
        {
            return Err(format!("damaged commit {name}: {e}"));
        }
    }
    Ok(())
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

#[derive(Debug, PartialEq, Eq)]
pub struct SyncReport {
    pub created: Vec<(ContentHash, String)>,
    pub branch: String,
}

/// Git Adapter: mirrors Alto commits that are not yet on the branch, oldest first.
/// Each Git commit's tree is the folder as it was at that Alto commit; objects are
/// only ever added, so later commits reuse earlier blobs.
pub fn sync(folder_dir: &Path, branch: Option<&str>) -> Result<SyncReport> {
    let p = Opened::from_entries(folder::read(folder_dir).map_err(|e| e.to_string())?)?;
    // Open skips a commit the engine would also drop. Importing it would
    // write the author line into fast-import, so sync still refuses.
    reject_damaged_commits(&p.entries)?;
    let prefix = git(folder_dir, &["rev-parse", "--show-prefix"])?
        .trim()
        .to_string();
    let root = PathBuf::from(git(folder_dir, &["rev-parse", "--show-toplevel"])?.trim());
    let current = git(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let branch = branch
        .map(str::to_string)
        .or_else(|| current.as_ref().map(|s| s.trim().to_string()))
        .unwrap_or_else(|| "main".into());
    check_branch_name(&branch)?;
    let reference = format!("refs/heads/{branch}");
    let tip = git(&root, &["rev-parse", "--verify", "--quiet", &reference])
        .ok()
        .map(|s| s.trim().to_string());
    let mirrored: BTreeSet<String> = match &tip {
        Some(t) => git(
            &root,
            &["log", "--format=%(trailers:key=Alto-Commit,valueonly)", t],
        )?
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect(),
        None => BTreeSet::new(),
    };
    let todo: Vec<ContentHash> = p
        .chain()
        .into_iter()
        .filter(|h| !mirrored.contains(&h.hex()))
        .collect();
    if todo.is_empty() {
        return Ok(SyncReport {
            created: vec![],
            branch,
        });
    }
    let mut present: BTreeSet<String> = match &tip {
        Some(t) => git(&root, &["ls-tree", "-r", "--name-only", t, "--", &prefix])?
            .lines()
            .filter_map(|l| l.strip_prefix(&prefix).map(str::to_string))
            .collect(),
        None => BTreeSet::new(),
    };
    let available: BTreeSet<String> = p
        .entries
        .keys()
        .filter(|k| k.starts_with("objects/"))
        .cloned()
        .collect();
    path_token("path", &prefix)?;
    let name = git(&root, &["config", "user.name"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "Alto".into());
    let email = git(&root, &["config", "user.email"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "alto@localhost".into());
    identity("committer", &name, false)?;
    identity("email", &email, true)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut stream: Vec<u8> = vec![];
    let mut created = vec![];
    let file = |stream: &mut Vec<u8>, path: &str, data: &[u8]| {
        let _ = write!(
            stream,
            "M 100644 inline {prefix}{path}\ndata {}\n",
            data.len()
        );
        stream.extend_from_slice(data);
        stream.push(b'\n');
    };
    for (n, h) in todo.iter().enumerate() {
        let c = &p.commits[h];
        let snap_bytes = if c.snapshot == ContentHash::of(&p.package.snapshot) {
            p.package.snapshot.clone()
        } else {
            p.package
                .snapshots
                .get(&c.snapshot)
                .cloned()
                .ok_or_else(|| format!("commit {} の snapshot がフォルダにありません", short(h)))?
        };
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
            None => Some(empty_like(&doc)),
            Some(ph) => p
                .commits
                .get(ph)
                .and_then(|pc| p.snapshot_doc(&pc.snapshot).ok()),
        };
        if let Some(parent) = parent_doc {
            let lines = render_diff(&parent, &doc, &alto_doc::diff(&parent, &doc));
            if !lines.is_empty() {
                message += &format!(
                    "\n\n{}",
                    lines.into_iter().take(200).collect::<Vec<_>>().join("\n")
                );
            }
        }
        message += &format!("\n\nAlto-Commit: {h}\nAlto-Snapshot: {}\n", c.snapshot);
        let who = c.author.clone().unwrap_or_else(|| name.clone());
        identity("author", &who, false)?;
        let _ = write!(stream, "commit {reference}\nmark :{}\n", n + 1);
        let _ = writeln!(
            stream,
            "author {who} <{email}> {} +0000",
            c.created_at.div_euclid(1000)
        );
        let _ = writeln!(stream, "committer {name} <{email}> {now} +0000");
        let _ = write!(stream, "data {}\n{message}\n", message.len());
        if n == 0
            && let Some(t) = &tip
        {
            let _ = writeln!(stream, "from {t}");
        }
        let mut files: Vec<(String, Vec<u8>)> = vec![];
        for o in referenced(&snap_bytes, &available) {
            if !present.contains(&o) {
                files.push((o.clone(), p.entries[&o].clone()));
            }
        }
        let mut needed = BTreeSet::new();
        for n in doc.nodes.values() {
            if let NodeKind::Raster { tiles } = &n.kind {
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
        files.push((alto_store::snapshot_path(&c.snapshot), snap_bytes.clone()));
        files.push((alto_store::commit_path(h), p.package.commits[h].clone()));
        files.push((
            "manifest.json".into(),
            alto_store::manifest_bytes(c.snapshot, Some(*h)),
        ));
        files.push(("document.json".into(), snap_bytes.clone()));
        files.push((
            ".gitattributes".into(),
            alto_store::GITATTRIBUTES.as_bytes().to_vec(),
        ));
        for (path, data) in files {
            if alto_store::is_immutable(&path) && present.contains(&path) {
                continue;
            }
            path_token("path", &path)?;
            if alto_store::is_immutable(&path) && !alto_store::bytes_match_name(&path, &data) {
                return Err(format!("content does not match {path}"));
            }
            file(&mut stream, &path, &data);
            present.insert(path);
        }
        stream.push(b'\n');
        created.push((*h, c.message.lines().next().unwrap_or("").to_string()));
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["fast-import", "--quiet", "--done"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    stream.extend_from_slice(b"done\n");
    child
        .stdin
        .take()
        .ok_or("git fast-import stdin")?
        .write_all(&stream)
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git fast-import: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    // The checked-out branch moved under the index: re-read only the package folder.
    if current.as_deref().map(str::trim) == Some(branch.as_str()) {
        let path = if prefix.is_empty() {
            "."
        } else {
            prefix.as_str()
        };
        git(&root, &["reset", "-q", "--", path])?;
    }
    Ok(SyncReport { created, branch })
}

/// Registers the textconv diff driver used by `.gitattributes` (`document.json diff=alto`).
pub fn install(repo: &Path, exe: &str) -> Result<()> {
    git(
        repo,
        &["config", "diff.alto.textconv", &format!("{exe} textconv")],
    )?;
    git(repo, &["config", "diff.alto.cachetextconv", "false"])?;
    git(
        repo,
        &["config", "diff.alto.xfuncname", r"^ *\[[a-z_]+\] .*$"],
    )?;
    Ok(())
}

/// `.alto` → folder form.
pub fn export(file: &Path, dir: &Path) -> Result<folder::FolderReport> {
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    let p = Opened::from_entries(alto_store::read_entries(&bytes).map_err(|e| e.to_string())?)?;
    let parts = parts(&p.package);
    let entries = alto_store::folder_entries(&parts).map_err(|e| e.to_string())?;
    let expect = std::fs::read(dir.join("manifest.json"))
        .ok()
        .map(|b| ContentHash::of(&b));
    folder::write(dir, &entries, expect).map_err(|e| e.to_string())
}
/// Folder form → `.alto`, keeping only objects reachable from the document and history.
pub fn pack(dir: &Path, file: &Path) -> Result<()> {
    let p = Opened::from_entries(folder::read(dir).map_err(|e| e.to_string())?)?;
    let mut parts = parts(&p.package);
    let available: BTreeSet<String> = p
        .entries
        .keys()
        .filter(|k| k.starts_with("objects/"))
        .cloned()
        .collect();
    let mut keep = referenced(&parts.snapshot, &available);
    for s in parts.snapshots.values() {
        keep.extend(referenced(s, &available));
    }
    parts
        .objects
        .retain(|h, _| keep.contains(&alto_store::object_path(h)));
    let bytes = alto_store::encode_parts(&parts).map_err(|e| e.to_string())?;
    let tmp = file.with_extension("alto.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, file).map_err(|e| e.to_string())
}
fn parts(p: &Package) -> alto_store::PackageParts {
    alto_store::PackageParts {
        snapshot: p.snapshot.clone(),
        objects: p.objects.clone(),
        snapshots: p.snapshots.clone(),
        commits: p.commits.clone(),
        head: p.head,
    }
}
