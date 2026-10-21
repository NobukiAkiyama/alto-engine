//! Mirrors an engine-made folder package into a real Git repository.
use alto_core::{PixelFormat, TileKey};
use alto_doc::{DocumentMeta, Node, NodeKind, Op, PropPatch, TileChange};
use alto_engine::{DocHandle, Engine};
use alto_raster::TileData;
use std::path::{Path, PathBuf};

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
    let NodeKind::Raster { tiles } = &doc.node(layer).unwrap().kind else {
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
    alto_git::install(&root, env!("CARGO_BIN_EXE_alto-git")).unwrap();
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
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    manifest_bytes.push(b'\n');
    std::fs::write(manifest_path, manifest_bytes).unwrap();
}

#[test]
fn sync_rejects_author_and_branch_injection() {
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
fn open_keeps_a_document_whose_author_sync_refuses() {
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
            .all(|c| c.author.as_deref() != Some("Alice <a@b.c>"))
    );
    let err = alto_git::sync(&folder, None).unwrap_err();
    assert!(err.contains("author"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn sync_rejects_a_spaced_path_and_a_mismatched_object() {
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
    assert!(err.contains("content does not match"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}
