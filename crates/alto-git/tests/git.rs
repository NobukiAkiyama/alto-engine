//! Mirrors an engine-made folder package into a real Git repository.
use alto_core::{PixelFormat, TileKey};
use alto_doc::{DocumentMeta, Node, NodeKind, Op, PropPatch, TileChange};
use alto_engine::{DocHandle, Engine};
use alto_raster::TileData;
use std::path::{Path, PathBuf};

/// A child process forked by another test thread briefly inherits the package lease,
/// so tests that hold leases and tests that spawn git run one at a time.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn git(dir: &Path, args: &[&str]) -> String {
    alto_git::git(dir, args).unwrap()
}
fn repo(name: &str) -> Option<PathBuf> {
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("git not installed; skipping");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("alto-git-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.name", "Alto Test"]);
    git(&dir, &["config", "user.email", "alto@example.invalid"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.md"), "artwork\n").unwrap();
    git(&dir, &["add", "README.md"]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    Some(dir)
}
fn paint(e: &mut Engine, d: DocHandle, layer: alto_core::NodeId, key: TileKey, v: u8) {
    let hash = e
        .put_tile(TileData::new(PixelFormat::Rgba8, vec![v; 262144]).unwrap())
        .unwrap();
    let doc = e.document(d).unwrap();
    let NodeKind::Raster { tiles, .. } = &doc.node(layer).unwrap().kind else {
        panic!()
    };
    let before = tiles.tiles.get(&key).copied();
    let rev = doc.revision;
    e.transact(
        d,
        "ブラシ",
        vec![Op::SetTiles {
            layer,
            changes: vec![TileChange {
                key,
                before,
                after: Some(hash),
            }],
        }],
        rev,
    )
    .unwrap();
}
fn save(e: &Engine, d: DocHandle, dir: &Path) {
    let entries = e.folder_entries(d).unwrap();
    let expect = std::fs::read(dir.join("manifest.json"))
        .ok()
        .map(|b| alto_core::ContentHash::of(&b));
    alto_store::folder::write(dir, &entries, expect).unwrap();
}

#[test]
fn folder_packages_are_managed_by_git_with_layer_level_diffs() {
    let _serial = serial();
    let Some(root) = repo("sync") else { return };
    let folder = root.join("art.altoproj");
    let mut e = Engine::new();
    let d = e
        .create_document(DocumentMeta::srgb_u8(1024, 1024))
        .unwrap();
    let (bg, ink) = (Node::raster("背景"), Node::raster("線画"));
    let (ib, ii) = (bg.id, ink.id);
    let r = e.document(d).unwrap().root;
    e.transact(
        d,
        "レイヤー追加",
        vec![
            Op::InsertNode {
                node: bg,
                parent: r,
                index: 0,
            },
            Op::InsertNode {
                node: ink,
                parent: r,
                index: 1,
            },
        ],
        0,
    )
    .unwrap();
    paint(&mut e, d, ib, TileKey { tx: 0, ty: 0 }, 9);
    e.commit(d, "下地を作成", 1_760_000_000_000, None, false)
        .unwrap();
    for v in [3, 4, 5] {
        paint(&mut e, d, ii, TileKey { tx: 2, ty: 1 }, v);
    }
    let rev = e.document(d).unwrap().revision;
    e.transact(
        d,
        "不透明度",
        vec![Op::SetProps {
            id: ib,
            patch: PropPatch {
                opacity: Some(0.5),
                ..Default::default()
            },
        }],
        rev,
    )
    .unwrap();
    let c2 = e
        .commit(
            d,
            "線画を描く",
            1_760_000_060_000,
            Some("mimi".into()),
            false,
        )
        .unwrap();
    save(&e, d, &folder);

    let report = alto_git::sync(&folder, None).unwrap();
    assert_eq!(report.created.len(), 2);
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]).trim(), "3");
    assert_eq!(
        git(&root, &["status", "--porcelain"]).trim(),
        "",
        "working tree matches the mirrored head"
    );
    git(&root, &["fsck", "--strict", "--no-progress"]);
    let msg = git(&root, &["log", "-1", "--format=%B"]);
    assert!(msg.starts_with("線画を描く"), "{msg}");
    assert!(msg.contains("- ブラシ ×3（線画）"), "{msg}");
    assert!(
        msg.contains("~ 画素    線画: 1タイル 文書座標 (512,256)-(768,512)"),
        "{msg}"
    );
    assert!(msg.contains("~ 属性    背景: opacity"), "{msg}");
    assert!(msg.contains(&format!("Alto-Commit: {}", c2.hash)));
    assert_eq!(
        git(&root, &["log", "-1", "--format=%an %at"]).trim(),
        "mimi 1760000060"
    );
    // The first mirrored commit is the folder as it was then: its manifest names that version.
    let first: alto_store::Manifest =
        serde_json::from_str(&git(&root, &["show", "HEAD~1:art.altoproj/manifest.json"])).unwrap();
    assert_ne!(first.head, Some(c2.hash));
    // Layer-level diffs between Git revisions.
    let a = alto_git::Opened::from_git(&folder, "HEAD~1").unwrap();
    let b = alto_git::Opened::from_git(&folder, "HEAD").unwrap();
    let text = alto_git::diff_text(&a.doc, &b.doc);
    assert!(
        text.contains("線画: 1タイル") && text.contains("背景: opacity"),
        "{text}"
    );
    // textconv makes plain `git diff` hunks name layers and tiles.
    let alto_git_bin = std::env::var("CARGO_BIN_EXE_alto-git")
        .expect("Cargo sets CARGO_BIN_EXE_alto-git while running integration tests");
    alto_git::install(&root, &alto_git_bin).unwrap();
    let plain = git(
        &root,
        &["diff", "HEAD~1", "HEAD", "--", "art.altoproj/document.json"],
    );
    assert!(
        plain.contains("[raster] 線画") && plain.contains("+    tile 2,1"),
        "{plain}"
    );
    let log =
        alto_git::log_text(&alto_git::Opened::open(folder.to_str().unwrap()).unwrap()).unwrap();
    assert!(
        log.contains("下地を作成") && log.contains("+ 追加    [raster] 背景"),
        "{log}"
    );

    // Idempotent; a new Alto commit adds exactly one Git commit and only new objects.
    assert!(alto_git::sync(&folder, None).unwrap().created.is_empty());
    paint(&mut e, d, ib, TileKey { tx: 3, ty: 3 }, 77);
    e.commit(d, "背景に追記", 1_760_000_120_000, None, false)
        .unwrap();
    save(&e, d, &folder);
    assert_eq!(alto_git::sync(&folder, None).unwrap().created.len(), 1);
    let changed = git(&root, &["diff", "--name-only", "HEAD~1", "HEAD"]);
    assert_eq!(
        changed.lines().filter(|l| l.contains("/objects/")).count(),
        1,
        "{changed}"
    );
    assert_eq!(git(&root, &["status", "--porcelain"]).trim(), "");
    // The folder converts to a single file and back with history intact.
    let file = root.join("art.alto");
    alto_git::pack(&folder, &file).unwrap();
    let mut fresh = Engine::new();
    let (f, warnings) = fresh.open_package(&std::fs::read(&file).unwrap()).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(fresh.log(f).unwrap().len(), 3);
    let out = root.join("copy.altoproj");
    alto_git::export(&file, &out).unwrap();
    assert_eq!(
        std::fs::read(out.join("document.json")).unwrap(),
        std::fs::read(folder.join("document.json")).unwrap()
    );
    let _ = std::fs::remove_dir_all(&root);
}

fn one_commit(root: &Path) -> PathBuf {
    let folder = root.join("art.altoproj");
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(64, 64)).unwrap();
    e.commit(d, "開始", 1_760_000_000_000, Some("mimi".into()), false)
        .unwrap();
    save(&e, d, &folder);
    folder
}

fn poison_author(folder: &Path, author: &str) {
    let manifest_path = folder.join("manifest.json");
    let mut manifest: alto_store::Manifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let old = manifest.head.unwrap();
    let mut commit: serde_json::Value =
        serde_json::from_slice(&std::fs::read(folder.join(format!("commits/{old}.json"))).unwrap())
            .unwrap();
    commit["author"] = serde_json::json!(author);
    let mut bytes = serde_json::to_vec_pretty(&commit).unwrap();
    bytes.push(b'\n');
    let hash = alto_core::ContentHash::of(&bytes);
    std::fs::write(folder.join(format!("commits/{hash}.json")), &bytes).unwrap();
    manifest.head = Some(hash);
    for b in manifest.refs.branches.values_mut() {
        if b.head == old {
            b.head = hash;
        }
    }
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    manifest_bytes.push(b'\n');
    std::fs::write(manifest_path, manifest_bytes).unwrap();
}

#[test]
fn sync_rejects_author_and_branch_injection() {
    let _serial = serial();
    let Some(root) = repo("inject") else { return };
    let folder = one_commit(&root);
    let head = git(&root, &["rev-parse", "HEAD"]);
    let branch = alto_git::sync(&folder, Some("main\nreset refs/heads/pwned")).unwrap_err();
    assert!(branch.contains("branch"), "{branch}");
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert!(
        alto_git::git(
            &root,
            &["rev-parse", "--verify", "--quiet", "refs/heads/pwned"]
        )
        .is_err()
    );

    poison_author(
        &folder,
        "Mallory\nreset refs/heads/pwned\nfrom refs/heads/main",
    );
    let author = alto_git::sync(&folder, None).unwrap_err();
    assert!(author.contains("author"), "{author}");
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    assert!(
        alto_git::git(
            &root,
            &["rev-parse", "--verify", "--quiet", "refs/heads/pwned"]
        )
        .is_err()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_keeps_an_angled_author_and_sync_strips_it() {
    let _serial = serial();
    let Some(root) = repo("author-open") else {
        return;
    };
    let folder = one_commit(&root);
    poison_author(&folder, "Alice <a@b.c>");
    let opened = alto_git::Opened::open(folder.to_str().unwrap()).unwrap();
    assert!(
        opened.warnings.iter().any(|w| w.contains("author")),
        "{:?}",
        opened.warnings
    );
    assert!(
        opened
            .commits
            .values()
            .any(|c| c.author.as_deref() == Some("Alice <a@b.c>"))
    );
    alto_git::sync(&folder, None).unwrap();
    let author = git(&root, &["log", "-1", "--format=%an"]);
    assert!(author.contains("Alice"), "{author}");
    assert!(!author.contains('<') && !author.contains('>'), "{author}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn sync_rejects_a_spaced_path_and_a_mismatched_object() {
    let _serial = serial();
    let Some(root) = repo("path-object") else {
        return;
    };
    let spaced = root.join("my art.altoproj");
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(64, 64)).unwrap();
    let layer = Node::raster("Ink");
    let id = layer.id;
    let parent = e.document(d).unwrap().root;
    e.transact(
        d,
        "layer",
        vec![Op::InsertNode {
            node: layer,
            parent,
            index: 0,
        }],
        0,
    )
    .unwrap();
    paint(&mut e, d, id, TileKey { tx: 0, ty: 0 }, 9);
    e.commit(d, "開始", 1_760_000_000_000, Some("mimi".into()), false)
        .unwrap();
    save(&e, d, &spaced);
    let err = alto_git::sync(&spaced, None).unwrap_err();
    assert!(err.contains("path"), "{err}");

    let folder = root.join("art.altoproj");
    save(&e, d, &folder);
    let mut flipped = false;
    for entry in std::fs::read_dir(folder.join("objects")).unwrap().flatten() {
        if entry.path().is_dir() {
            for file in std::fs::read_dir(entry.path()).unwrap().flatten() {
                let mut bytes = std::fs::read(file.path()).unwrap();
                if let Some(b) = bytes.last_mut() {
                    *b ^= 0xff;
                }
                std::fs::write(file.path(), bytes).unwrap();
                flipped = true;
            }
        }
    }
    assert!(flipped);
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(
        err.contains("CORRUPT_DATA") || err.contains("content does not match"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Paints on `layer` and records one commit with a stable, per-call timestamp.
fn commit_on(
    e: &mut Engine,
    d: DocHandle,
    layer: alto_core::NodeId,
    key: TileKey,
    v: u8,
    message: &str,
) -> alto_core::ContentHash {
    paint(e, d, layer, key, v);
    let at = 1_760_000_000_000 + i64::from(v) * 1000;
    e.commit(d, message, at, Some("mimi".into()), false)
        .unwrap()
        .hash
}

struct Forked {
    engine: Engine,
    doc: DocHandle,
    layer: alto_core::NodeId,
    start: alto_core::ContentHash,
    feature: alto_core::ContentHash,
    main: alto_core::ContentHash,
}

/// main: start -- main; feature: start -- feature.
fn forked() -> Forked {
    let mut engine = Engine::new();
    let doc = engine
        .create_document(DocumentMeta::srgb_u8(1024, 1024))
        .unwrap();
    let node = Node::raster("Ink");
    let layer = node.id;
    let parent = engine.document(doc).unwrap().root;
    engine
        .transact(
            doc,
            "layer",
            vec![Op::InsertNode {
                node,
                parent,
                index: 0,
            }],
            0,
        )
        .unwrap();
    // The image range is fixed here, so that the two sides differ only in pixels.
    paint(&mut engine, doc, layer, TileKey { tx: 3, ty: 3 }, 8);
    let start = commit_on(
        &mut engine,
        doc,
        layer,
        TileKey { tx: 0, ty: 0 },
        1,
        "start",
    );
    engine.create_branch(doc, "feature", None, true).unwrap();
    let feature = commit_on(
        &mut engine,
        doc,
        layer,
        TileKey { tx: 1, ty: 0 },
        2,
        "feature work",
    );
    engine.checkout(doc, "main").unwrap();
    let main = commit_on(
        &mut engine,
        doc,
        layer,
        TileKey { tx: 2, ty: 0 },
        3,
        "main work",
    );
    Forked {
        engine,
        doc,
        layer,
        start,
        feature,
        main,
    }
}

fn merge(f: &mut Forked, name: &str, message: &str) -> alto_core::ContentHash {
    let plan = f.engine.prepare_merge(f.doc, name).unwrap();
    assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
    f.engine
        .finish_merge(f.doc, &plan.token, vec![], message, 1_760_000_900_000, None)
        .unwrap()
        .head
}

fn parents(root: &Path, rev: &str) -> Vec<String> {
    git(root, &["rev-list", "--parents", "-1", rev])
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn git_commit_of(root: &Path, reference: &str, alto: alto_core::ContentHash) -> String {
    let found = git(
        root,
        &[
            "log",
            "--format=%H",
            &format!("--grep=^Alto-Commit: {alto}$"),
            reference,
        ],
    );
    let lines: Vec<&str> = found.lines().collect();
    assert_eq!(lines.len(), 1, "one Git commit for {alto}: {found}");
    lines[0].to_string()
}

fn snapshot_refs(root: &Path) -> String {
    git(root, &["for-each-ref", "--format=%(refname) %(objectname)"])
}

/// A Git revision's tree, as a package a fresh engine has to open.
fn open_tree(root: &Path, rev: &str, name: &str) -> (Engine, DocHandle) {
    let out = root.join(format!("tree-{}", rev.replace('/', "-")));
    std::fs::create_dir_all(&out).unwrap();
    let archive = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["archive", "--format=tar", rev, name])
        .output()
        .unwrap();
    assert!(archive.status.success());
    let tar = out.join("tree.tar");
    std::fs::write(&tar, archive.stdout).unwrap();
    assert!(
        std::process::Command::new("tar")
            .arg("-xf")
            .arg(&tar)
            .arg("-C")
            .arg(&out)
            .status()
            .unwrap()
            .success()
    );
    let entries = alto_store::folder::read(&out.join(name)).unwrap();
    let bytes = alto_store::write_entries(&entries).unwrap();
    let mut fresh = Engine::new();
    let (d, warnings) = fresh.open_package(&bytes).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let _ = std::fs::remove_dir_all(&out);
    (fresh, d)
}

#[test]
fn branches_and_merges_keep_their_shape_in_git() {
    let _serial = serial();
    let Some(root) = repo("dag") else { return };
    let folder = root.join("art.altoproj");
    let mut f = forked();
    save(&f.engine, f.doc, &folder);
    let before_base = git(&root, &["rev-parse", "main"]);

    let first = alto_git::sync(&folder, None).unwrap();
    assert_eq!(first.created.len(), 3);
    assert_eq!(first.branch, "main");
    let main_start = git_commit_of(&root, "main", f.start);
    let main_tip = git_commit_of(&root, "main", f.main);
    let feature_tip = git_commit_of(&root, "alto/feature", f.feature);
    assert_eq!(git(&root, &["rev-parse", "main"]).trim(), main_tip);
    assert_eq!(
        git(&root, &["rev-parse", "alto/feature"]).trim(),
        feature_tip
    );
    assert_eq!(parents(&root, &main_tip), std::slice::from_ref(&main_start));
    assert_eq!(
        parents(&root, &feature_tip),
        std::slice::from_ref(&main_start)
    );
    assert_eq!(
        parents(&root, &main_start),
        [before_base.trim().to_string()]
    );
    git(&root, &["fsck", "--strict", "--no-progress"]);

    // A second sync changes nothing, not even the state ref.
    let refs = snapshot_refs(&root);
    let again = alto_git::sync(&folder, None).unwrap();
    assert!(
        again.created.is_empty() && again.refs.is_empty(),
        "{again:?}"
    );
    assert_eq!(snapshot_refs(&root), refs);

    // A merge is one Git commit with the Alto parents in the same order.
    f.engine.set_tag(f.doc, "v1", Some(f.feature)).unwrap();
    let merged = merge(&mut f, "feature", "merge feature");
    let on_main = commit_on(
        &mut f.engine,
        f.doc,
        f.layer,
        TileKey { tx: 3, ty: 0 },
        4,
        "after merge",
    );
    f.engine.pin_version(f.doc, f.start, true).unwrap();
    save(&f.engine, f.doc, &folder);
    let synced = alto_git::sync(&folder, None).unwrap();
    assert_eq!(
        synced.created.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
        [merged, on_main]
    );
    let merge_git = git_commit_of(&root, "main", merged);
    assert_eq!(
        parents(&root, &merge_git),
        [main_tip.clone(), feature_tip.clone()],
        "first parent is ours"
    );
    assert_eq!(
        git(&root, &["merge-base", &main_tip, &feature_tip]).trim(),
        main_start
    );
    assert_eq!(
        git(&root, &["rev-list", "--count", "--first-parent", "main"]).trim(),
        "5",
        "init, start, main work, merge, after merge"
    );
    let body = git(&root, &["log", "-1", "--format=%B", &merge_git]);
    assert!(body.contains("Alto-Merge-Base:"), "{body}");
    assert_eq!(
        git(&root, &["rev-parse", "refs/tags/alto/v1"]).trim(),
        feature_tip
    );
    git(&root, &["fsck", "--strict", "--no-progress"]);
    assert_eq!(git(&root, &["status", "--porcelain"]).trim(), "");
    println!(
        "{}",
        git(
            &root,
            &[
                "log",
                "--graph",
                "--format=%h %s%d",
                "--all",
                "--exclude=refs/alto/*"
            ]
        )
    );

    // Each ref's tree is a package the engine can open, with the whole ancestry.
    let (fresh, d) = open_tree(&root, "main", "art.altoproj");
    assert_eq!(fresh.log(d).unwrap()[0].hash, on_main);
    assert_eq!(fresh.log(d).unwrap().len(), 4, "first-parent history");
    assert_eq!(
        fresh.log_dag(d, false).unwrap().len(),
        5,
        "all parents are in the tree"
    );
    assert_eq!(
        fresh.version_refs(d).unwrap().current_branch.as_deref(),
        Some("main")
    );
    let (fresh, d) = open_tree(&root, "alto/feature", "art.altoproj");
    assert_eq!(fresh.log(d).unwrap()[0].hash, f.feature);
    let refs = fresh.version_refs(d).unwrap();
    assert_eq!(refs.current_branch, None, "not the head when imported");
    assert!(
        refs.branches.is_empty(),
        "no branch name that could go stale"
    );

    // Reading through Git follows both parents.
    let opened = alto_git::Opened::from_git(&folder, "main").unwrap();
    let log = alto_git::log_text(&opened).unwrap();
    assert!(
        log.contains("feature work") && log.contains("マージ "),
        "{log}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unexpected_git_ref_is_a_conflict_and_nothing_moves() {
    let _serial = serial();
    let Some(root) = repo("conflict") else { return };
    let folder = root.join("art.altoproj");
    let mut f = forked();
    save(&f.engine, f.doc, &folder);
    alto_git::sync(&folder, None).unwrap();
    let synced = snapshot_refs(&root);
    commit_on(
        &mut f.engine,
        f.doc,
        f.layer,
        TileKey { tx: 3, ty: 3 },
        9,
        "next",
    );
    save(&f.engine, f.doc, &folder);

    // Somebody commits on the mirrored branch.
    let tip = git(&root, &["rev-parse", "main"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "by hand"]);
    let moved = snapshot_refs(&root);
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(
        err.starts_with("CONFLICT") && err.contains("refs/heads/main"),
        "{err}"
    );
    assert_eq!(snapshot_refs(&root), moved, "no ref changed");
    git(&root, &["reset", "-q", "--soft", tip.trim()]);

    // A moved or deleted side branch is a conflict too.
    let feature = git(&root, &["rev-parse", "alto/feature"]);
    git(&root, &["branch", "-f", "alto/feature", "main~1"]);
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(err.contains("refs/heads/alto/feature"), "{err}");
    git(&root, &["branch", "-D", "alto/feature"]);
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(err.contains("no ref"), "{err}");
    git(&root, &["branch", "alto/feature", feature.trim()]);
    assert_eq!(snapshot_refs(&root), synced);

    // Put back, the same sync lands and adds only the new version.
    let done = alto_git::sync(&folder, None).unwrap();
    assert_eq!(done.created.len(), 1);
    git(&root, &["fsck", "--strict", "--no-progress"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_new_ref_never_takes_over_an_existing_git_branch() {
    let _serial = serial();
    let Some(root) = repo("existing") else { return };
    let folder = root.join("art.altoproj");
    let f = forked();
    save(&f.engine, f.doc, &folder);
    git(&root, &["branch", "alto/feature"]);
    let before = snapshot_refs(&root);
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(
        err.starts_with("CONFLICT") && err.contains("already exists"),
        "{err}"
    );
    assert_eq!(snapshot_refs(&root), before);
    git(&root, &["branch", "-D", "alto/feature"]);

    // A ref that fails only inside the transaction also leaves nothing behind:
    // `refs/heads/alto` makes `refs/heads/alto/feature` impossible.
    git(&root, &["branch", "alto"]);
    let before = snapshot_refs(&root);
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(err.starts_with("CONFLICT"), "{err}");
    let after = snapshot_refs(&root);
    assert_eq!(after, before);
    git(&root, &["branch", "-D", "alto"]);
    assert_eq!(alto_git::sync(&folder, None).unwrap().created.len(), 3);
    git(&root, &["fsck", "--strict", "--no-progress"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_damaged_commit_table_is_refused() {
    let _serial = serial();
    let Some(root) = repo("table") else { return };
    let folder = root.join("art.altoproj");
    let f = forked();
    save(&f.engine, f.doc, &folder);
    alto_git::sync(&folder, None).unwrap();
    let state_ref = git(
        &root,
        &["for-each-ref", "--format=%(refname)", "refs/alto/*/state"],
    );
    let state_ref = state_ref.trim();
    let original = git(&root, &["cat-file", "blob", state_ref]);
    let write_state = |text: &str| {
        let path = root.join("state.json");
        std::fs::write(&path, text).unwrap();
        let blob = git(&root, &["hash-object", "-w", path.to_str().unwrap()]);
        std::fs::remove_file(path).unwrap();
        git(&root, &["update-ref", state_ref, blob.trim()]);
    };

    // The table says the feature commit is the start commit's Git commit.
    let mut state: serde_json::Value = serde_json::from_str(&original).unwrap();
    let start_git = state["commits"][f.start.hex()].clone();
    state["commits"][f.feature.hex()] = start_git;
    write_state(&serde_json::to_string(&state).unwrap());
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(
        err.starts_with("CONFLICT") && err.contains("mapping"),
        "{err}"
    );

    // An unknown field or format is not read as if it were understood.
    let mut state: serde_json::Value = serde_json::from_str(&original).unwrap();
    state["format"] = serde_json::json!(99);
    write_state(&serde_json::to_string(&state).unwrap());
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(err.starts_with("UNSUPPORTED"), "{err}");
    // An edited table cannot point a sync at some other ref.
    let mut state: serde_json::Value = serde_json::from_str(&original).unwrap();
    state["branches"]["feature"]["git_ref"] = serde_json::json!("refs/heads/release");
    write_state(&serde_json::to_string(&state).unwrap());
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(err.contains("unexpected entry"), "{err}");
    let mut state: serde_json::Value = serde_json::from_str(&original).unwrap();
    state["extra"] = serde_json::json!(1);
    write_state(&serde_json::to_string(&state).unwrap());
    assert!(
        alto_git::sync(&folder, None)
            .unwrap_err()
            .starts_with("CONFLICT")
    );

    // Dropping the table must not import the same versions a second time.
    git(&root, &["update-ref", "-d", state_ref]);
    let count = git(&root, &["rev-list", "--count", "--all"]);
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(err.contains("already in this repository"), "{err}");
    assert_eq!(git(&root, &["rev-list", "--count", "--all"]), count);

    write_state(&original);
    assert!(alto_git::sync(&folder, None).unwrap().created.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn deleting_an_alto_branch_only_removes_refs_alto_git_created() {
    let _serial = serial();
    let Some(root) = repo("delete") else { return };
    let folder = root.join("art.altoproj");
    let mut f = forked();
    f.engine
        .create_branch(f.doc, "other", Some(f.start), false)
        .unwrap();
    save(&f.engine, f.doc, &folder);
    alto_git::sync(&folder, None).unwrap();

    // `other` was created by the sync and points at an ancestor: the ref goes.
    f.engine.delete_branch(f.doc, "other", false).unwrap();
    save(&f.engine, f.doc, &folder);
    let done = alto_git::sync(&folder, None).unwrap();
    assert!(done.created.is_empty());
    assert_eq!(
        done.refs,
        [alto_git::RefUpdate {
            git_ref: "refs/heads/alto/other".into(),
            commit: None
        }]
    );
    assert!(
        alto_git::git(
            &root,
            &["rev-parse", "--verify", "--quiet", "refs/heads/alto/other"]
        )
        .is_err()
    );

    // Removed by force and re-created elsewhere, the name would have to move back.
    f.engine.delete_branch(f.doc, "feature", true).unwrap();
    f.engine
        .create_branch(f.doc, "feature", Some(f.start), false)
        .unwrap();
    save(&f.engine, f.doc, &folder);
    let before = snapshot_refs(&root);
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(err.contains("no longer descends"), "{err}");
    assert_eq!(snapshot_refs(&root), before);

    // Deleted and synced, the ref goes and the same name can start again.
    f.engine.delete_branch(f.doc, "feature", true).unwrap();
    save(&f.engine, f.doc, &folder);
    alto_git::sync(&folder, None).unwrap();
    f.engine
        .create_branch(f.doc, "feature", Some(f.start), false)
        .unwrap();
    save(&f.engine, f.doc, &folder);
    alto_git::sync(&folder, None).unwrap();
    assert_eq!(
        git(&root, &["rev-parse", "alto/feature"]).trim(),
        git_commit_of(&root, "main", f.start)
    );
    git(&root, &["fsck", "--strict", "--no-progress"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mappings_stay_fixed_and_packages_do_not_share_refs() {
    let _serial = serial();
    let Some(root) = repo("packages") else { return };
    let folder = root.join("art.altoproj");
    let f = forked();
    save(&f.engine, f.doc, &folder);
    alto_git::sync(&folder, None).unwrap();
    let err = alto_git::sync(&folder, Some("other")).unwrap_err();
    assert!(err.contains("already mapped"), "{err}");
    let err = alto_git::sync_with(
        &folder,
        alto_git::SyncOptions {
            branch: None,
            ref_prefix: Some("art"),
        },
    )
    .unwrap_err();
    assert!(err.contains("ref prefix"), "{err}");

    // A second document cannot land on a ref the first one maps, but can use its own.
    let other = root.join("other.altoproj");
    let second = forked();
    save(&second.engine, second.doc, &other);
    let err = alto_git::sync(&other, None).unwrap_err();
    assert!(err.contains("mapped by document"), "{err}");
    git(&root, &["branch", "second", "main"]);
    let done = alto_git::sync_with(
        &other,
        alto_git::SyncOptions {
            branch: Some("second"),
            ref_prefix: Some("second-art"),
        },
    )
    .unwrap();
    assert_eq!(done.created.len(), 3);
    assert_eq!(
        git(&root, &["rev-parse", "second-art/feature"]).trim(),
        git_commit_of(&root, "second-art/feature", second.feature)
    );
    git(&root, &["fsck", "--strict", "--no-progress"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn branch_names_that_differ_only_by_case_are_refused() {
    let _serial = serial();
    let Some(root) = repo("case") else { return };
    let folder = root.join("art.altoproj");
    let mut f = forked();
    f.engine
        .create_branch(f.doc, "FEATURE", None, false)
        .unwrap();
    save(&f.engine, f.doc, &folder);
    let before = snapshot_refs(&root);
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(
        err.starts_with("UNSUPPORTED") && err.contains("case"),
        "{err}"
    );
    assert_eq!(snapshot_refs(&root), before);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_detached_head_and_pins_stay_reachable() {
    let _serial = serial();
    let Some(root) = repo("detached") else { return };
    let folder = root.join("art.altoproj");
    let mut f = forked();
    f.engine.detach(f.doc, f.feature).unwrap();
    save(&f.engine, f.doc, &folder);
    alto_git::sync(&folder, None).unwrap();
    let keep = git(
        &root,
        &["for-each-ref", "--format=%(refname)", "refs/alto/*/keep/*"],
    );
    assert!(keep.contains(&f.feature.hex()), "{keep}");
    git(&root, &["fsck", "--strict", "--no-progress"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_checked_out_branch_is_not_deleted_and_a_pruned_commit_is_imported_again() {
    let _serial = serial();
    let dir = std::env::temp_dir().join(format!("alto-git-unborn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.name", "Alto Test"]);
    git(&dir, &["config", "user.email", "alto@example.invalid"]);
    let folder = dir.join("art.altoproj");
    let mut f = forked();
    save(&f.engine, f.doc, &folder);
    alto_git::sync(&folder, None).unwrap();

    // main was created by the sync and Git has it checked out.
    f.engine.protect_branch(f.doc, "main", false).unwrap();
    f.engine.checkout(f.doc, "feature").unwrap();
    f.engine.delete_branch(f.doc, "main", true).unwrap();
    save(&f.engine, f.doc, &folder);
    let done = alto_git::sync(&folder, None).unwrap();
    assert!(
        done.warnings.iter().any(|w| w.contains("checked out")),
        "{done:?}"
    );
    git(&dir, &["rev-parse", "--verify", "HEAD"]);

    // A branch that left Alto, pruned from Git, and then back.
    f.engine.create_branch(f.doc, "side", None, true).unwrap();
    let side = commit_on(
        &mut f.engine,
        f.doc,
        f.layer,
        TileKey { tx: 1, ty: 1 },
        30,
        "side",
    );
    save(&f.engine, f.doc, &folder);
    alto_git::sync(&folder, None).unwrap();
    f.engine.checkout(f.doc, "feature").unwrap();
    f.engine.delete_branch(f.doc, "side", true).unwrap();
    save(&f.engine, f.doc, &folder);
    alto_git::sync(&folder, None).unwrap();
    git(&dir, &["reflog", "expire", "--expire=now", "--all"]);
    git(&dir, &["gc", "-q", "--prune=now"]);
    f.engine
        .create_branch(f.doc, "side", Some(side), false)
        .unwrap();
    save(&f.engine, f.doc, &folder);
    let again = alto_git::sync(&folder, None).unwrap();
    assert_eq!(again.created.len(), 1);
    git(&dir, &["fsck", "--strict", "--no-progress"]);
    let _ = std::fs::remove_dir_all(&dir);
}
