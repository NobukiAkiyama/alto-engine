use alto_core::ContentHash;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read, Write},
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("CORRUPT_DATA: {0}")]
    Corrupt(String),
    #[error("STORAGE: {0}")]
    Io(String),
}
type Result<T> = std::result::Result<T, StoreError>;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub snapshot: ContentHash,
    pub creator: String,
    /// Latest version commit (M8). Absent for documents without history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<ContentHash>,
}
pub const CREATOR: &str = "Alto Engine 0.4.0";
pub struct Package {
    pub snapshot: Vec<u8>,
    pub objects: BTreeMap<ContentHash, Vec<u8>>,
    pub warnings: Vec<String>,
    pub opaque: std::collections::BTreeSet<ContentHash>,
    /// Snapshots of version commits, verified against their names.
    pub snapshots: BTreeMap<ContentHash, Vec<u8>>,
    /// Commit bytes, verified against their names.
    pub commits: BTreeMap<ContentHash, Vec<u8>>,
    pub head: Option<ContentHash>,
}
/// Everything a package contains, before choosing the single-file or folder form.
#[derive(Default)]
pub struct PackageParts {
    pub snapshot: Vec<u8>,
    pub objects: BTreeMap<ContentHash, Vec<u8>>,
    pub snapshots: BTreeMap<ContentHash, Vec<u8>>,
    pub commits: BTreeMap<ContentHash, Vec<u8>>,
    pub head: Option<ContentHash>,
}
pub fn object_path(h: &ContentHash) -> String {
    format!("objects/{}/{}", &h.hex()[..2], h)
}
pub fn snapshot_path(h: &ContentHash) -> String {
    format!("snapshots/{h}.json")
}
pub fn commit_path(h: &ContentHash) -> String {
    format!("commits/{h}.json")
}
/// Files that never change once written (content-addressed).
pub fn is_immutable(path: &str) -> bool {
    path.starts_with("objects/") || path.starts_with("snapshots/") || path.starts_with("commits/")
}
/// True when `data` is the bytes named by a content-addressed path.
/// The object shard has to be the first two hex digits of that hash.
/// Native only: wasm32 does not save folders or run `git fast-import`.
#[cfg(not(target_arch = "wasm32"))]
pub fn bytes_match_name(name: &str, data: &[u8]) -> bool {
    if let Some(rest) = name.strip_prefix("objects/") {
        let mut parts = rest.split('/');
        let (Some(shard), Some(hash), None) = (
            parts.next(),
            parts.next().and_then(|s| s.parse::<ContentHash>().ok()),
            parts.next(),
        ) else {
            return false;
        };
        if shard != &hash.hex()[..2] {
            return false;
        }
        return decode_object(data)
            .map(|(tag, pixels)| object_hash(tag, &pixels) == hash)
            .unwrap_or(false);
    }
    let Some(stem) = name
        .strip_prefix("snapshots/")
        .or_else(|| name.strip_prefix("commits/"))
        .and_then(|s| s.strip_suffix(".json"))
    else {
        return false;
    };
    stem.parse::<ContentHash>()
        .map(|hash| ContentHash::of(data) == hash)
        .unwrap_or(false)
}
pub const GITATTRIBUTES: &str = "# Alto folder package. Objects are compressed, content-addressed and never rewritten.\nobjects/** binary\n*.json text eol=lf\ndocument.json diff=alto\n";
/// Logical entries shared by the ZIP and folder forms; manifest.json is the commit point.
pub fn package_entries(parts: &PackageParts) -> Result<BTreeMap<String, Vec<u8>>> {
    let hash = ContentHash::of(&parts.snapshot);
    let mut entries = BTreeMap::new();
    for (h, data) in &parts.snapshots {
        entries.insert(snapshot_path(h), data.clone());
    }
    entries.insert(snapshot_path(&hash), parts.snapshot.clone());
    for (h, data) in &parts.commits {
        entries.insert(commit_path(h), data.clone());
    }
    for (h, data) in &parts.objects {
        entries.insert(object_path(h), data.clone());
    }
    entries.insert("manifest.json".into(), manifest_bytes(hash, parts.head));
    Ok(entries)
}
/// Canonical manifest bytes for a snapshot and head commit.
pub fn manifest_bytes(snapshot: ContentHash, head: Option<ContentHash>) -> Vec<u8> {
    let manifest = Manifest {
        format: 1,
        snapshot,
        creator: CREATOR.into(),
        head,
    };
    let mut m = serde_json::to_vec_pretty(&manifest).expect("manifest serializes");
    m.push(b'\n');
    m
}
/// Folder form: the package plus a readable mirror of the current snapshot for Git diffs.
pub fn folder_entries(parts: &PackageParts) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut e = package_entries(parts)?;
    e.insert("document.json".into(), parts.snapshot.clone());
    e.insert(".gitattributes".into(), GITATTRIBUTES.as_bytes().to_vec());
    Ok(e)
}
pub fn encode_parts(parts: &PackageParts) -> Result<Vec<u8>> {
    write_entries(&package_entries(parts)?)
}
pub fn encode_object(tag: u8, data: &[u8]) -> Vec<u8> {
    let mut out = b"ALTOOBJ1".to_vec();
    out.push(tag);
    out.extend(lz4_flex::compress_prepend_size(data));
    out
}
pub fn decode_object(bytes: &[u8]) -> Result<(u8, Vec<u8>)> {
    if bytes.len() < 13 || &bytes[..8] != b"ALTOOBJ1" {
        return Err(StoreError::Corrupt("object header".into()));
    }
    let size = u32::from_le_bytes(
        bytes[9..13]
            .try_into()
            .map_err(|_| StoreError::Corrupt("size".into()))?,
    ) as usize;
    if (bytes[8] == 4 && size != 262144)
        || (bytes[8] == 1 && size != 65536)
        || ![0, 1, 4].contains(&bytes[8])
    {
        return Err(StoreError::Corrupt("object format/size".into()));
    }
    if size > 268_435_456 {
        return Err(StoreError::Corrupt("object too large".into()));
    }
    let data = lz4_flex::decompress_size_prepended(&bytes[9..])
        .map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok((bytes[8], data))
}
pub fn object_hash(tag: u8, data: &[u8]) -> ContentHash {
    if tag == 0 {
        ContentHash::of(data)
    } else {
        ContentHash::of_tagged(tag, data)
    }
}
pub fn write_entries(entries: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for (name, data) in entries {
        zip.start_file(name, options)
            .map_err(|e| StoreError::Io(e.to_string()))?;
        zip.write_all(data)
            .map_err(|e| StoreError::Io(e.to_string()))?;
    }
    Ok(zip
        .finish()
        .map_err(|e| StoreError::Io(e.to_string()))?
        .into_inner())
}
/// Uncompressed package cap. A declared size above this is refused before the
/// bytes are buffered, and a reader that keeps going is stopped at the same cap.
pub const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;

fn read_limited(reader: &mut impl Read, budget: u64) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        if n == 0 {
            break;
        }
        if data.len() as u64 + n as u64 > budget {
            return Err(StoreError::Corrupt("package exceeds 512 MiB".into()));
        }
        data.extend_from_slice(&buf[..n]);
    }
    Ok(data)
}

pub fn read_entries(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    if archive.len() > 1_000_000 {
        return Err(StoreError::Corrupt("too many entries".into()));
    }
    let mut entries = BTreeMap::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut f = archive
            .by_index(i)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let budget = MAX_PACKAGE_BYTES.saturating_sub(total);
        if f.size() > budget {
            return Err(StoreError::Corrupt("package exceeds 512 MiB".into()));
        }
        let name = f.name().to_string();
        if name.contains("..") || name.starts_with('/') || entries.contains_key(&name) {
            return Err(StoreError::Corrupt("invalid or duplicate path".into()));
        }
        // `size()` is the header. A lying small size must not fill the cap.
        let data = read_limited(&mut f, budget)?;
        total += data.len() as u64;
        entries.insert(name, data);
    }
    Ok(entries)
}
pub fn encode_package(snapshot: &[u8], objects: BTreeMap<ContentHash, Vec<u8>>) -> Result<Vec<u8>> {
    encode_parts(&PackageParts {
        snapshot: snapshot.to_vec(),
        objects,
        ..Default::default()
    })
}
pub fn decode_package(bytes: &[u8]) -> Result<Package> {
    decode_entries(read_entries(bytes)?)
}
fn named(entries: &BTreeMap<String, Vec<u8>>, dir: &str) -> BTreeMap<ContentHash, Vec<u8>> {
    entries
        .iter()
        .filter_map(|(n, b)| {
            let h = n
                .strip_prefix(dir)?
                .strip_suffix(".json")?
                .parse::<ContentHash>()
                .ok()?;
            (ContentHash::of(b) == h).then(|| (h, b.clone()))
        })
        .collect()
}
/// Reads the logical entries of either form. Damaged parts become warnings.
pub fn decode_entries(mut entries: BTreeMap<String, Vec<u8>>) -> Result<Package> {
    let mut warnings = Vec::new();
    let manifest = entries
        .get("manifest.json")
        .and_then(|b| serde_json::from_slice::<Manifest>(b).ok())
        .filter(|m| m.format == 1);
    let mut snapshots = named(&entries, "snapshots/");
    let commits = named(&entries, "commits/");
    let damaged = |dir: &str, ok: &BTreeMap<ContentHash, Vec<u8>>| {
        entries
            .keys()
            .filter(|n| n.starts_with(dir) && !ok.keys().any(|h| n.contains(&h.hex())))
            .map(|n| format!("damaged {n}"))
            .collect::<Vec<_>>()
    };
    warnings.extend(damaged("snapshots/", &snapshots));
    warnings.extend(damaged("commits/", &commits));
    let mut head = manifest.as_ref().and_then(|m| m.head);
    let mut snapshot = manifest
        .as_ref()
        .and_then(|m| snapshots.get(&m.snapshot).cloned());
    if snapshot.is_none() {
        warnings.push("manifest damaged; recovered a valid snapshot".into());
        // Prefer the newest commit whose snapshot survived, then any valid snapshot.
        let newest = commits
            .iter()
            .filter_map(|(h, b)| {
                let v: serde_json::Value = serde_json::from_slice(b).ok()?;
                let s: ContentHash = v.get("snapshot")?.as_str()?.parse().ok()?;
                let t = v.get("created_at")?.as_i64()?;
                snapshots.contains_key(&s).then_some((t, *h, s))
            })
            .max();
        head = newest.map(|n| n.1);
        snapshot = newest
            .and_then(|n| snapshots.get(&n.2).cloned())
            .or_else(|| snapshots.values().next_back().cloned());
    }
    if let Some(h) = head
        && !commits.contains_key(&h)
    {
        warnings.push(format!("missing head commit {h}"));
    }
    let snapshot = snapshot.ok_or_else(|| StoreError::Corrupt("no valid snapshot".into()))?;
    snapshots.remove(&ContentHash::of(&snapshot));
    entries.retain(|n, _| n.starts_with("objects/"));
    let mut objects = BTreeMap::new();
    let mut opaque = std::collections::BTreeSet::new();
    for (name, data) in entries {
        if let Some(hash) = name
            .strip_prefix("objects/")
            .and_then(|s| s.split('/').nth(1))
            .and_then(|s| s.parse::<ContentHash>().ok())
        {
            match decode_object(&data) {
                Ok((tag, pixels)) if object_hash(tag, &pixels) == hash => {
                    if tag == 4 && pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255) {
                        opaque.insert(hash);
                    }
                    objects.insert(hash, data);
                }
                _ => warnings.push(format!("damaged object {hash}")),
            }
        }
    }
    Ok(Package {
        snapshot,
        objects,
        warnings,
        opaque,
        snapshots,
        commits,
        head,
    })
}
/// The folder form (`.altoproj/`) on a native file system. Content-addressed files stay
/// when their bytes match the name, and are replaced when they do not. `manifest.json`
/// is replaced last and read back.
#[cfg(not(target_arch = "wasm32"))]
pub mod folder {
    use super::*;
    use std::path::Path;

    #[derive(Debug, Default, PartialEq, Eq)]
    pub struct FolderReport {
        pub written: usize,
        pub skipped: usize,
    }
    fn io(e: impl std::fmt::Display) -> StoreError {
        StoreError::Io(e.to_string())
    }
    fn replace(dir: &Path, name: &str, data: &[u8]) -> Result<()> {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let tmp = dir.join(format!(".{}.tmp", name.replace('/', "_")));
        {
            let mut f = std::fs::File::create(&tmp).map_err(io)?;
            f.write_all(data).map_err(io)?;
            f.sync_all().map_err(io)?;
        }
        std::fs::rename(&tmp, &path).map_err(io)
    }
    fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && !name.starts_with('/')
            && name
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != "..")
    }
    /// Writes entries. `expect` is the hash of the manifest this save was based on
    /// (optimistic lock); a different manifest on disk is a conflict.
    pub fn write(
        dir: &Path,
        entries: &BTreeMap<String, Vec<u8>>,
        expect: Option<ContentHash>,
    ) -> Result<FolderReport> {
        std::fs::create_dir_all(dir).map_err(io)?;
        let current = std::fs::read(dir.join("manifest.json")).ok();
        if let Some(expect) = expect
            && current.as_deref().map(ContentHash::of) != Some(expect)
        {
            return Err(StoreError::Io(
                "CONFLICT: manifest changed since it was read".into(),
            ));
        }
        let manifest = entries
            .get("manifest.json")
            .ok_or_else(|| StoreError::Io("manifest missing".into()))?;
        let mut report = FolderReport::default();
        for (name, data) in entries
            .iter()
            .filter(|(n, _)| n.as_str() != "manifest.json")
        {
            if !valid_name(name) {
                return Err(StoreError::Io(format!("invalid path {name}")));
            }
            let path = dir.join(name);
            if is_immutable(name) {
                if !bytes_match_name(name, data) {
                    return Err(StoreError::Corrupt(format!(
                        "content does not match {name}"
                    )));
                }
                if path.exists() {
                    let existing = std::fs::read(&path).map_err(io)?;
                    // Same bytes already matched the name. A different encoding is
                    // kept only when it still hashes to that name.
                    if existing == *data || bytes_match_name(name, &existing) {
                        report.skipped += 1;
                        continue;
                    }
                }
            }
            replace(dir, name, data)?;
            report.written += 1;
        }
        replace(dir, "manifest.json", manifest)?;
        report.written += 1;
        if std::fs::read(dir.join("manifest.json")).map_err(io)? != *manifest {
            return Err(StoreError::Io("manifest verification failed".into()));
        }
        Ok(report)
    }
    /// Reads the package files (manifest, snapshots, commits, objects).
    pub fn read(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut out = BTreeMap::new();
        let manifest = std::fs::read(dir.join("manifest.json")).map_err(io)?;
        out.insert("manifest.json".to_string(), manifest);
        for sub in ["snapshots", "commits", "objects"] {
            let root = dir.join(sub);
            if !root.is_dir() {
                continue;
            }
            let mut stack = vec![root];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d).map_err(io)? {
                    let p = e.map_err(io)?.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if let Ok(rel) = p.strip_prefix(dir) {
                        let name = rel.to_string_lossy().replace('\\', "/");
                        if !name.rsplit('/').next().is_some_and(|f| f.starts_with('.')) {
                            out.insert(name, std::fs::read(&p).map_err(io)?);
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}
pub trait ObjectStore {
    fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>>;
    fn put(&mut self, hash: ContentHash, data: Vec<u8>) -> Result<()>;
}
pub trait RefStore {
    fn read_ref(&self, name: &str) -> Result<Option<Vec<u8>>>;
    fn write_ref(&mut self, name: &str, data: Vec<u8>, expect: Option<ContentHash>) -> Result<()>;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_and_zip_share_entries_and_history() {
        let pixels = vec![7; 262144];
        let h = object_hash(4, &pixels);
        let commit = b"{\"snapshot\":\"x\",\"created_at\":1}\n".to_vec();
        let ch = ContentHash::of(&commit);
        let old = b"{\"old\":true}\n".to_vec();
        let parts = PackageParts {
            snapshot: b"{\"nodes\":[]}\n".to_vec(),
            objects: BTreeMap::from([(h, encode_object(4, &pixels))]),
            snapshots: BTreeMap::from([(ContentHash::of(&old), old.clone())]),
            commits: BTreeMap::from([(ch, commit.clone())]),
            head: Some(ch),
        };
        let zip = decode_package(&encode_parts(&parts).unwrap()).unwrap();
        assert_eq!(zip.head, Some(ch));
        assert_eq!(zip.commits[&ch], commit);
        assert_eq!(
            zip.snapshots.len(),
            1,
            "current snapshot is not repeated as history"
        );
        let dir = std::env::temp_dir().join(format!("alto-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let entries = folder_entries(&parts).unwrap();
        let first = folder::write(&dir, &entries, None).unwrap();
        assert_eq!(first.skipped, 0);
        assert!(dir.join("document.json").exists() && dir.join(".gitattributes").exists());
        let manifest = ContentHash::of(&std::fs::read(dir.join("manifest.json")).unwrap());
        let second = folder::write(&dir, &entries, Some(manifest)).unwrap();
        assert_eq!(
            second.skipped, 4,
            "objects, snapshots and commits are not rewritten"
        );
        assert!(folder::write(&dir, &entries, Some(ContentHash::of(b"stale"))).is_err());
        let read = decode_entries(folder::read(&dir).unwrap()).unwrap();
        assert_eq!(read.snapshot, parts.snapshot);
        assert_eq!(read.head, Some(ch));
        assert_eq!(decode_object(&read.objects[&h]).unwrap().1, pixels);
        // A damaged manifest falls back to the newest commit's snapshot when it exists.
        std::fs::write(dir.join("manifest.json"), b"{").unwrap();
        let read = decode_entries(folder::read(&dir).unwrap()).unwrap();
        assert!(read.warnings.iter().any(|w| w.contains("manifest damaged")));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_corrupt_object_is_replaced_by_the_bytes_that_match_its_name() {
        let pixels = vec![7; 262144];
        let h = object_hash(4, &pixels);
        let commit = b"{\"snapshot\":\"x\",\"created_at\":1}\n".to_vec();
        let ch = ContentHash::of(&commit);
        let parts = PackageParts {
            snapshot: b"{\"nodes\":[]}\n".to_vec(),
            objects: BTreeMap::from([(h, encode_object(4, &pixels))]),
            commits: BTreeMap::from([(ch, commit)]),
            head: Some(ch),
            ..Default::default()
        };
        let dir = std::env::temp_dir().join(format!("alto-folder-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let entries = folder_entries(&parts).unwrap();
        folder::write(&dir, &entries, None).unwrap();
        let path = dir.join(object_path(&h));
        let original = std::fs::read(&path).unwrap();
        let mut damaged = original.clone();
        damaged[20] ^= 0xff;
        std::fs::write(&path, &damaged).unwrap();
        let manifest = ContentHash::of(&std::fs::read(dir.join("manifest.json")).unwrap());
        folder::write(&dir, &entries, Some(manifest)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let mut forged = entries.clone();
        forged.get_mut(&object_path(&h)).unwrap()[20] ^= 0xff;
        let elsewhere =
            std::env::temp_dir().join(format!("alto-folder-forged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&elsewhere);
        assert!(folder::write(&elsewhere, &forged, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }
    #[test]
    fn object_name_requires_the_shard_prefix() {
        let pixels = vec![7; 262144];
        let h = object_hash(4, &pixels);
        let data = encode_object(4, &pixels);
        assert!(bytes_match_name(&object_path(&h), &data));
        assert!(!bytes_match_name(&format!("objects/zz/{h}"), &data));
        assert!(!bytes_match_name(
            &format!("objects/{}/{h}/extra", &h.hex()[..2]),
            &data
        ));
    }
    #[test]
    fn stored_zip_and_object_hash_roundtrip() {
        let pixels = vec![41; 262144];
        let h = object_hash(4, &pixels);
        let obj = encode_object(4, &pixels);
        let bytes = encode_package(br#"{"nodes":[]}"#, BTreeMap::from([(h, obj)])).unwrap();
        let package = decode_package(&bytes).unwrap();
        assert_eq!(decode_object(&package.objects[&h]).unwrap().1, pixels);
        assert!(package.warnings.is_empty());
    }
    #[test]
    fn bad_object_is_reported_without_losing_document() {
        let mut e = BTreeMap::new();
        let snapshot = b"{}";
        let h = ContentHash::of(snapshot);
        e.insert(format!("snapshots/{h}.json"), snapshot.to_vec());
        let bad = ContentHash::of(b"bad");
        e.insert(
            format!("objects/{}/{bad}", &bad.hex()[..2]),
            b"damaged".to_vec(),
        );
        let p = decode_package(&write_entries(&e).unwrap()).unwrap();
        assert_eq!(p.snapshot, snapshot);
        assert_eq!(p.warnings.len(), 2);
    }
    #[test]
    fn read_limited_stops_before_buffering_the_cap() {
        let mut reader = std::io::repeat(1u8).take(10_000);
        let err = read_limited(&mut reader, 100).unwrap_err();
        assert!(err.to_string().contains("512 MiB"), "{err}");
    }
}
