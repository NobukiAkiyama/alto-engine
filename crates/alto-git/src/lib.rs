//! Git Adapter for Alto folder packages (`.altoproj/`).
//!
//! The engine never links Git. This crate reads the folder form, mirrors each Alto
//! commit into one Git commit with `git fast-import`, and renders the semantic diff
//! layer by layer for terminals and `git diff` (textconv). Native only.
#![cfg(not(target_arch = "wasm32"))]
use alto_core::ContentHash;
use alto_doc::{Commit, DiffItem, Document, NodeKind, version::ContentDetail};
use alto_store::{Package, folder};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

mod mirror;
pub use mirror::{RefUpdate, SyncOptions, SyncReport, sync, sync_with};

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
pub(crate) fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
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
    /// Commits that stayed readable but whose author cannot be copied onto a
    /// git author line. `sync` keeps the version and strips `<` and `>`.
    pub warnings: Vec<String>,
}
impl Opened {
    pub fn from_entries(entries: BTreeMap<String, Vec<u8>>) -> Result<Self> {
        let package = alto_store::decode_entries(entries.clone()).map_err(|e| e.to_string())?;
        let doc = Document::from_snapshot_bytes(&package.snapshot).map_err(|e| e.to_string())?;
        let mut commits = BTreeMap::new();
        let mut warnings = Vec::new();
        for (h, b) in &package.commits {
            match Commit::parse_history(b) {
                Ok((c, warning)) => {
                    if let Some(w) = warning {
                        warnings.push(format!("commit {h}: {w}"));
                    }
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
        let mut stack: Vec<ContentHash> = m.head.into_iter().collect();
        while let Some(h) = stack.pop() {
            let path = alto_store::commit_path(&h);
            if entries.contains_key(&path) {
                continue;
            }
            let Ok(bytes) = show(&path) else { continue };
            let (c, _) = Commit::parse_history(&bytes)?;
            if let Ok(s) = show(&alto_store::snapshot_path(&c.snapshot)) {
                entries.insert(alto_store::snapshot_path(&c.snapshot), s);
            }
            entries.insert(path, bytes);
            stack.extend(c.parents.iter().copied());
        }
        Self::from_entries(entries)
    }
    /// Every commit reachable from the head through all parents, parents first.
    pub fn history(&self) -> Vec<ContentHash> {
        let mut out = vec![];
        let mut seen = BTreeSet::new();
        let mut stack: Vec<(ContentHash, bool)> =
            self.package.head.into_iter().map(|h| (h, false)).collect();
        while let Some((h, expanded)) = stack.pop() {
            if expanded {
                out.push(h);
            } else if seen.insert(h)
                && let Some(c) = self.commits.get(&h)
            {
                stack.push((h, true));
                stack.extend(c.parents.iter().rev().map(|p| (*p, false)));
            }
        }
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
pub(crate) fn short(h: &ContentHash) -> String {
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
                format!("~ 内容    [{kind}] {name}: {}", describe(detail))
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

/// The engine reports what changed as data; this command line words it.
fn describe(detail: &ContentDetail) -> String {
    match detail {
        ContentDetail::Geometry => "image bounds".into(),
        ContentDetail::Vector { paths, commands } => format!(
            "パス {}本/{}命令 → {}本/{}命令",
            paths[0], commands[0], paths[1], commands[1]
        ),
        ContentDetail::Text {
            text,
            style,
            writing_mode,
            frame,
            tate_chu_yoko,
        } => {
            let mut parts = vec![];
            if let Some([from, to]) = text {
                parts.push(format!("「{from}」→「{to}」"));
            }
            if *style {
                parts.push("書式".into());
            }
            match writing_mode {
                Some(alto_text::WritingMode::VerticalRl) => parts.push("縦組みへ".into()),
                Some(alto_text::WritingMode::HorizontalTb) => parts.push("横組みへ".into()),
                None => {}
            }
            if *frame {
                parts.push("枠・揃え".into());
            }
            if *tate_chu_yoko {
                parts.push("縦中横".into());
            }
            parts.join("、")
        }
        ContentDetail::Adjustment => "調整値".into(),
        ContentDetail::Unsupported => "未対応ノードの内容".into(),
        ContentDetail::KindChanged { from, to } => format!("{from} → {to}"),
    }
}

pub(crate) fn intent_lines(doc: &Document, c: &Commit) -> Vec<String> {
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
            let name = i.label.as_deref().unwrap_or(&i.intent);
            let name = if i.undone {
                format!("取り消し: {name}")
            } else {
                name.to_string()
            };
            if targets.is_empty() {
                format!("- {name}{count}")
            } else {
                format!("- {name}{count}（{}）", targets.join("、"))
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
    let history = p.history();
    for h in history.iter().rev() {
        let c = &p.commits[h];
        let doc = p.snapshot_doc(&c.snapshot)?;
        s += &format!(
            "commit {}  {}\n",
            short(h),
            c.message.lines().next().unwrap_or("")
        );
        if let [ours, theirs] = c.parents.as_slice() {
            s += &format!("マージ {} {}\n", short(ours), short(theirs));
        }
        if let Some(a) = &c.author {
            s += &format!("作成者 {a}\n");
        }
        for l in intent_lines(&doc, c) {
            s += &format!("  {l}\n");
        }
        let parent = match c.parents.first().and_then(|ph| p.commits.get(ph)) {
            Some(pc) => p.snapshot_doc(&pc.snapshot)?,
            None => empty_like(&doc),
        };
        for l in render_diff(&parent, &doc, &alto_doc::diff(&parent, &doc)) {
            s += &format!("    {l}\n");
        }
        s.push('\n');
    }
    if history.is_empty() {
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
            NodeKind::Raster { tiles, .. } => {
                for (k, h) in &tiles.tiles {
                    *s += &format!("{pad}  tile {k} {}\n", short(h));
                }
            }
            NodeKind::Vector { paths } => {
                for (i, p) in paths.iter().enumerate() {
                    *s += &format!("{pad}  path {i}: {}\n", p.data.to_svg());
                }
            }
            NodeKind::Text { content, .. } => {
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
            NodeKind::Adjustment { adjustment } => {
                *s += &format!(
                    "{pad}  {}\n",
                    serde_json::to_string(adjustment).unwrap_or_else(|_| "adjustment".into())
                );
            }
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
pub(crate) fn referenced(snapshot: &[u8], available: &BTreeSet<String>) -> BTreeSet<String> {
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
    let target = alto_store::archive::lock(file).map_err(|e| e.to_string())?;
    let expected = target
        .read()
        .map_err(|e| e.to_string())?
        .as_ref()
        .map(|b| alto_store::sha256_hex(b))
        .unwrap_or_default();
    target.replace(&bytes, &expected).map_err(|e| e.to_string())
}
fn parts(p: &Package) -> alto_store::PackageParts {
    alto_store::PackageParts {
        snapshot: p.snapshot.clone(),
        objects: p.objects.clone(),
        snapshots: p.snapshots.clone(),
        commits: p.commits.clone(),
        head: p.head,
        refs: p.refs.clone(),
    }
}
