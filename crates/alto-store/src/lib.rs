#[cfg(not(target_arch = "wasm32"))]
pub mod archive;
use alto_core::ContentHash;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read, Write},
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("UNSUPPORTED: {0}")]
    Unsupported(String),
    #[error("CORRUPT_DATA: {0}")]
    Corrupt(String),
    #[error("STORAGE: {0}")]
    Io(String),
}
type Result<T> = std::result::Result<T, StoreError>;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub document_format: u32,
    pub snapshot: ContentHash,
    pub creator: String,
    /// Latest version commit (M8). Absent for documents without history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<ContentHash>,
    #[serde(flatten)]
    pub refs: VersionRefs,
    /// Present exactly in format 3: versions' objects live outside this .alto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_history: Option<ExternalHistory>,
}
/// Where a thin .alto's version objects are: a count and digest of `external-objects.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalHistory {
    pub document: alto_core::NodeId,
    pub objects: u64,
    pub digest: ContentHash,
}
/// Objects only versions reference, kept in the history store instead of the .alto.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalObjects {
    pub document: alto_core::NodeId,
    pub objects: std::collections::BTreeSet<ContentHash>,
}
pub const EXTERNAL_OBJECTS: &str = "external-objects.json";
/// Canonical bytes of the external list: a sorted JSON array of hashes.
pub fn external_objects_bytes(objects: &std::collections::BTreeSet<ContentHash>) -> Vec<u8> {
    serde_json::to_vec(objects).expect("hashes serialize")
}
/// Mutable references shared by archives, folders, and recovery checkpoints.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRefs {
    pub current_branch: Option<String>,
    pub branches: BTreeMap<String, Branch>,
    pub tags: BTreeMap<String, ContentHash>,
    pub pinned: std::collections::BTreeSet<ContentHash>,
    pub generation: u64,
    pub gc_generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    pub head: ContentHash,
    pub protected: bool,
}
impl VersionRefs {
    pub fn roots(&self) -> impl Iterator<Item = ContentHash> + '_ {
        self.branches
            .values()
            .map(|b| b.head)
            .chain(self.tags.values().copied())
            .chain(self.pinned.iter().copied())
    }
    pub fn validate(&self, head: Option<ContentHash>) -> Result<()> {
        if let Some(name) = &self.current_branch
            && (self.branches.get(name).map(|b| b.head) != head || head.is_none())
        {
            return Err(StoreError::Corrupt(
                "current branch does not match head".into(),
            ));
        }
        if head.is_none() && (self.current_branch.is_some() || self.roots().next().is_some()) {
            return Err(StoreError::Corrupt("references without a head".into()));
        }
        Ok(())
    }
}
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
    pub refs: VersionRefs,
    /// Some for a thin .alto (format 3).
    pub external: Option<ExternalObjects>,
}
/// Everything a package contains, before choosing the single-file or folder form.
#[derive(Clone, Default)]
pub struct PackageParts {
    pub snapshot: Vec<u8>,
    pub objects: BTreeMap<ContentHash, Vec<u8>>,
    pub snapshots: BTreeMap<ContentHash, Vec<u8>>,
    pub commits: BTreeMap<ContentHash, Vec<u8>>,
    pub head: Option<ContentHash>,
    pub refs: VersionRefs,
    /// Some: write a thin .alto (format 3) whose versions' objects are elsewhere.
    pub external: Option<ExternalObjects>,
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
    parts.refs.validate(parts.head)?;
    entries.insert(
        "manifest.json".into(),
        manifest_bytes_with_refs(
            hash,
            parts.head,
            parts.refs.clone(),
            parts.external.as_ref(),
        ),
    );
    if let Some(ext) = &parts.external {
        entries.insert(
            EXTERNAL_OBJECTS.into(),
            external_objects_bytes(&ext.objects),
        );
    }
    Ok(entries)
}
/// Canonical manifest bytes for a snapshot and head commit.
pub fn manifest_bytes(snapshot: ContentHash, head: Option<ContentHash>) -> Vec<u8> {
    manifest_bytes_with_refs(snapshot, head, VersionRefs::default(), None)
}
pub fn manifest_bytes_with_refs(
    snapshot: ContentHash,
    head: Option<ContentHash>,
    refs: VersionRefs,
    external: Option<&ExternalObjects>,
) -> Vec<u8> {
    let manifest = Manifest {
        format: if external.is_some() { 3 } else { 2 },
        external_history: external.map(|e| ExternalHistory {
            document: e.document,
            objects: e.objects.len() as u64,
            digest: ContentHash::of(&external_objects_bytes(&e.objects)),
        }),
        document_format: 5,
        refs,
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
    if parts.external.is_some() {
        return Err(StoreError::Io("a folder holds every object".into()));
    }
    let mut e = package_entries(parts)?;
    e.insert("document.json".into(), parts.snapshot.clone());
    e.insert(".gitattributes".into(), GITATTRIBUTES.as_bytes().to_vec());
    e.insert(
        ".gitignore".into(),
        b".alto.lock\n.alto-gc.json\n.*.tmp\n".to_vec(),
    );
    Ok(e)
}
pub fn encode_parts(parts: &PackageParts) -> Result<Vec<u8>> {
    encode_parts_fetching(parts, &BTreeMap::new(), |hash| {
        Err(StoreError::Io(format!("object {hash} was not supplied")))
    })
}

/// Where the bytes of one ZIP entry come from while a package is written.
enum Source<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
    Fetched(ContentHash, u64),
}

/// The ZIP form of `parts`, written without copying its entries first. The objects
/// in `fetched` (hash → encoded length) are not in `parts`: each is fetched when its
/// entry is written and dropped right after, so the engine never holds a second copy
/// of every object. The output buffer is sized up front, so it is not regrown while
/// it fills. The bytes are the same as `write_entries(&package_entries(parts))` with
/// the fetched objects added to `parts.objects`.
pub fn encode_parts_fetching<E: From<StoreError>>(
    parts: &PackageParts,
    fetched: &BTreeMap<ContentHash, u64>,
    mut fetch: impl FnMut(&ContentHash) -> std::result::Result<Vec<u8>, E>,
) -> std::result::Result<Vec<u8>, E> {
    let entries = package_sources(parts, fetched)?;
    let total = sources_len(&entries);
    if total > MAX_PACKAGE_BYTES {
        return Err(StoreError::Io("RESOURCE_LIMIT: package exceeds 512 MiB".into()).into());
    }
    let capacity = total as usize
        + entries
            .keys()
            .map(|name| ZIP_ENTRY_OVERHEAD + 2 * name.len())
            .sum::<usize>()
        + ZIP_END_OVERHEAD;
    let mut zip = ZipWriter::new(Cursor::new(Vec::with_capacity(capacity)));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let io = |e: &dyn std::fmt::Display| StoreError::Io(e.to_string());
    for (name, source) in &entries {
        zip.start_file(name.as_str(), options).map_err(|e| io(&e))?;
        match source {
            Source::Borrowed(b) => zip.write_all(b),
            Source::Owned(b) => zip.write_all(b),
            Source::Fetched(h, len) => {
                let bytes = fetch(h)?;
                if bytes.len() as u64 != *len {
                    return Err(StoreError::Io(format!("object {h} changed size")).into());
                }
                zip.write_all(&bytes)
            }
        }
        .map_err(|e| io(&e))?;
    }
    Ok(zip.finish().map_err(|e| io(&e))?.into_inner())
}
/// The bytes the package cap counts (the entries' data, without ZIP headers) for
/// `parts` plus objects of the given encoded lengths, without writing anything.
pub fn package_payload(parts: &PackageParts, fetched: &BTreeMap<ContentHash, u64>) -> Result<u64> {
    Ok(sources_len(&package_sources(parts, fetched)?))
}
fn sources_len(entries: &BTreeMap<String, Source>) -> u64 {
    entries
        .values()
        .map(|source| match source {
            Source::Borrowed(b) => b.len() as u64,
            Source::Owned(b) => b.len() as u64,
            Source::Fetched(_, len) => *len,
        })
        .sum()
}
fn package_sources<'a>(
    parts: &'a PackageParts,
    fetched: &BTreeMap<ContentHash, u64>,
) -> Result<BTreeMap<String, Source<'a>>> {
    parts.refs.validate(parts.head)?;
    let hash = ContentHash::of(&parts.snapshot);
    let mut entries: BTreeMap<String, Source> = BTreeMap::new();
    for (h, data) in &parts.snapshots {
        entries.insert(snapshot_path(h), Source::Borrowed(data));
    }
    entries.insert(snapshot_path(&hash), Source::Borrowed(&parts.snapshot));
    for (h, data) in &parts.commits {
        entries.insert(commit_path(h), Source::Borrowed(data));
    }
    for (h, &len) in fetched {
        debug_assert!(
            !parts.objects.contains_key(h),
            "{h} is both held and fetched"
        );
        entries.insert(object_path(h), Source::Fetched(*h, len));
    }
    for (h, data) in &parts.objects {
        entries.insert(object_path(h), Source::Borrowed(data));
    }
    entries.insert(
        "manifest.json".into(),
        Source::Owned(manifest_bytes_with_refs(
            hash,
            parts.head,
            parts.refs.clone(),
            parts.external.as_ref(),
        )),
    );
    if let Some(ext) = &parts.external {
        entries.insert(
            EXTERNAL_OBJECTS.into(),
            Source::Owned(external_objects_bytes(&ext.objects)),
        );
    }
    Ok(entries)
}
/// Bytes a stored ZIP entry adds besides its data and two copies of its name: the
/// local header (30) and the central directory record (46).
const ZIP_ENTRY_OVERHEAD: usize = 76;
/// The end of central directory record (22), plus the Zip64 record (56) and locator
/// (20) a package with more than 65,535 entries also gets.
const ZIP_END_OVERHEAD: usize = 128;
pub fn encode_object(tag: u8, data: &[u8]) -> Vec<u8> {
    let mut out = b"ALTOOBJ1".to_vec();
    out.push(tag);
    out.extend(lz4_flex::compress_prepend_size(data));
    out
}
pub fn decode_object(bytes: &[u8]) -> Result<(u8, Vec<u8>)> {
    let (tag, _) = object_header(bytes)?;
    let data = lz4_flex::decompress_size_prepended(&bytes[9..])
        .map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok((tag, data))
}
fn object_header(bytes: &[u8]) -> Result<(u8, usize)> {
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
    Ok((bytes[8], size))
}
pub fn object_hash(tag: u8, data: &[u8]) -> ContentHash {
    if tag == 0 {
        ContentHash::of(data)
    } else {
        ContentHash::of_tagged(tag, data)
    }
}
pub fn write_entries(entries: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    if entries.values().map(|b| b.len() as u64).sum::<u64>() > MAX_PACKAGE_BYTES {
        return Err(StoreError::Io(
            "RESOURCE_LIMIT: package exceeds 512 MiB".into(),
        ));
    }
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
pub fn decode_entries(entries: BTreeMap<String, Vec<u8>>) -> Result<Package> {
    decode_entries_impl(entries, None)
}
/// Only for an engine-created checkpoint whose complete ZIP and metadata were
/// verified against the journal digest. External packages use decode_entries.
pub fn decode_recovery_entries(
    entries: BTreeMap<String, Vec<u8>>,
    opaque: &std::collections::BTreeSet<ContentHash>,
) -> Result<Package> {
    decode_entries_impl(entries, Some(opaque))
}
fn decode_entries_impl(
    mut entries: BTreeMap<String, Vec<u8>>,
    trusted_opaque: Option<&std::collections::BTreeSet<ContentHash>>,
) -> Result<Package> {
    let mut warnings = Vec::new();
    if let Some(bytes) = entries.get("manifest.json")
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes)
        && (!matches!(
            (
                value.get("format").and_then(|v| v.as_u64()),
                value.get("external_history").is_some_and(|v| !v.is_null())
            ),
            (Some(2), false) | (Some(3), true)
        ) || value.get("document_format").and_then(|v| v.as_u64()) != Some(5))
    {
        return Err(StoreError::Unsupported("package format".into()));
    }
    let manifest = Some(
        alto_core::json::from_slice::<Manifest>(
            entries
                .get("manifest.json")
                .ok_or_else(|| StoreError::Corrupt("manifest missing".into()))?,
        )
        .map_err(|e| StoreError::Corrupt(format!("manifest: {e}")))?,
    );
    if let Some(m) = &manifest {
        m.refs.validate(m.head)?;
    }
    let refs = manifest
        .as_ref()
        .map(|m| m.refs.clone())
        .unwrap_or_default();
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
    let head = manifest.as_ref().and_then(|m| m.head);
    let snapshot = manifest
        .as_ref()
        .and_then(|m| snapshots.get(&m.snapshot).cloned());
    if snapshot.is_none() {
        return Err(StoreError::Corrupt(
            "manifest snapshot missing or corrupt".into(),
        ));
    }
    if let Some(h) = head
        && !commits.contains_key(&h)
    {
        warnings.push(format!("missing head commit {h}"));
    }
    let snapshot = snapshot.ok_or_else(|| StoreError::Corrupt("no valid snapshot".into()))?;
    snapshots.remove(&ContentHash::of(&snapshot));
    let external = match manifest.as_ref().and_then(|m| m.external_history.as_ref()) {
        Some(h) => Some(read_external(h, entries.get(EXTERNAL_OBJECTS))?),
        None => None,
    };
    entries.retain(|n, _| n.starts_with("objects/"));
    let mut objects = BTreeMap::new();
    let mut opaque = std::collections::BTreeSet::new();
    for (name, data) in entries {
        if let Some(hash) = name
            .strip_prefix("objects/")
            .and_then(|s| s.split('/').nth(1))
            .and_then(|s| s.parse::<ContentHash>().ok())
        {
            if let Some(known_opaque) = trusted_opaque {
                if name != object_path(&hash) {
                    return Err(StoreError::Corrupt(format!(
                        "invalid recovery object path {name}"
                    )));
                }
                let (tag, _) = object_header(&data)?;
                if known_opaque.contains(&hash) {
                    if tag != 4 {
                        return Err(StoreError::Corrupt(format!(
                            "opaque object is not RGBA {hash}"
                        )));
                    }
                    opaque.insert(hash);
                }
                objects.insert(hash, data);
            } else {
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
    }
    if let Some(ext) = &external
        && objects.keys().any(|h| ext.objects.contains(h))
    {
        return Err(StoreError::Corrupt(
            "external objects overlap the package".into(),
        ));
    }
    if trusted_opaque.is_some_and(|known| !known.is_subset(&opaque)) {
        return Err(StoreError::Corrupt(
            "recovery opaque index contains missing objects".into(),
        ));
    }
    if !warnings.is_empty() {
        return Err(StoreError::Corrupt(warnings.join("; ")));
    }
    Ok(Package {
        snapshot,
        objects,
        warnings,
        opaque,
        snapshots,
        commits,
        head,
        refs,
        external,
    })
}
fn read_external(h: &ExternalHistory, bytes: Option<&Vec<u8>>) -> Result<ExternalObjects> {
    let bad = |m: &str| StoreError::Corrupt(format!("external objects: {m}"));
    let bytes = bytes.ok_or_else(|| bad("missing"))?;
    if ContentHash::of(bytes) != h.digest {
        return Err(bad("digest mismatch"));
    }
    let list: Vec<ContentHash> = serde_json::from_slice(bytes).map_err(|e| bad(&e.to_string()))?;
    if list.len() as u64 != h.objects {
        return Err(bad("count mismatch"));
    }
    if !list.windows(2).all(|w| w[0] < w[1]) {
        return Err(bad("not sorted or has duplicates"));
    }
    Ok(ExternalObjects {
        document: h.document,
        objects: list.into_iter().collect(),
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
        pub warnings: Vec<String>,
    }
    fn io(e: impl std::fmt::Display) -> StoreError {
        StoreError::Io(e.to_string())
    }
    fn replace(dir: &Path, name: &str, data: &[u8]) -> Result<()> {
        if !valid_name(name) {
            return Err(io("invalid package path"));
        }
        let path = dir.join(name);
        let mut component = dir.to_path_buf();
        for part in name.split('/') {
            component.push(part);
            if std::fs::symlink_metadata(&component).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(io("symlink inside package"));
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let tmp = dir.join(format!(
            ".{}.{}.tmp",
            name.replace('/', "_"),
            alto_core::NodeId::new().0
        ));
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(io)?;
            f.write_all(data).map_err(io)?;
            f.sync_all().map_err(io)?;
        }
        std::fs::rename(&tmp, &path).map_err(io)?;
        std::fs::File::open(path.parent().unwrap_or(dir))
            .and_then(|f| f.sync_all())
            .map_err(io)
    }
    /// An OS-held lease. All native package reads, writes and GC use this lock.
    pub struct LockedFolder {
        dir: std::path::PathBuf,
        _lock: std::fs::File,
    }
    pub fn lock(dir: &Path) -> Result<LockedFolder> {
        std::fs::create_dir_all(dir).map_err(io)?;
        let path = dir.join(".alto.lock");
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(io("symlink lock"));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(io)?;
        file.try_lock()
            .map_err(|e| io(format!("BUSY: package lease: {e}")))?;
        Ok(LockedFolder {
            dir: dir.to_path_buf(),
            _lock: file,
        })
    }
    impl LockedFolder {
        pub fn read(&self) -> Result<BTreeMap<String, Vec<u8>>> {
            read_unlocked(&self.dir)
        }
        pub fn write(
            &self,
            entries: &BTreeMap<String, Vec<u8>>,
            expect: Option<ContentHash>,
        ) -> Result<FolderReport> {
            write_unlocked(&self.dir, entries, expect)
        }
        pub fn write_gc_record(&self, record: &[u8]) -> Result<()> {
            replace(&self.dir, ".alto-gc.json", record)
        }
        pub fn delete_immutable(&self, path: &str) -> Result<()> {
            if !valid_name(path) || !is_immutable(path) {
                return Err(io("invalid GC path"));
            }
            let p = self.dir.join(path);
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(e)),
            }
            if let Some(parent) = p.parent() {
                std::fs::File::open(parent)
                    .and_then(|f| f.sync_all())
                    .map_err(io)?;
            }
            Ok(())
        }
    }
    pub fn write(
        dir: &Path,
        entries: &BTreeMap<String, Vec<u8>>,
        expect: Option<ContentHash>,
    ) -> Result<FolderReport> {
        lock(dir)?.write(entries, expect)
    }
    pub fn read(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
        lock(dir)?.read()
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
    fn write_unlocked(
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
            .filter(|(n, _)| n.as_str() != "manifest.json" && n.as_str() != "document.json")
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
        if let Some(mirror) = entries.get("document.json") {
            match replace(dir, "document.json", mirror) {
                Ok(()) => report.written += 1,
                Err(e) => report
                    .warnings
                    .push(format!("document.json mirror needs repair: {e}")),
            }
        }
        Ok(report)
    }
    /// Reads the package files (manifest, snapshots, commits, objects).
    fn read_unlocked(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut out = BTreeMap::new();
        let manifest = std::fs::read(dir.join("manifest.json")).map_err(io)?;
        out.insert("manifest.json".to_string(), manifest);
        for sub in ["snapshots", "commits", "objects"] {
            let root = dir.join(sub);
            if std::fs::symlink_metadata(&root).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(io("symlink inside package"));
            }
            if !root.is_dir() {
                continue;
            }
            let mut stack = vec![root];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d).map_err(io)? {
                    let entry = e.map_err(io)?;
                    if entry.file_type().map_err(io)?.is_symlink() {
                        return Err(io("symlink inside package"));
                    }
                    let p = entry.path();
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
    fn sample_parts() -> PackageParts {
        let objects: BTreeMap<_, _> = (0..40u8)
            .map(|i| {
                let pixels: Vec<u8> = (0..65536u32).map(|j| (j as u8) ^ i).collect();
                let bytes = encode_object(1, &pixels);
                (object_hash(1, &pixels), bytes)
            })
            .collect();
        let old = b"{\"old\":1}".to_vec();
        PackageParts {
            snapshot: b"{\"doc\":1}".to_vec(),
            snapshots: [(ContentHash::of(&old), old)].into(),
            objects,
            ..Default::default()
        }
    }
    fn thin_parts() -> (PackageParts, std::collections::BTreeSet<ContentHash>) {
        let pixels = vec![9; 262144];
        let kept = object_hash(4, &pixels);
        let ext: std::collections::BTreeSet<_> =
            [ContentHash::of(b"a"), ContentHash::of(b"b")].into();
        let parts = PackageParts {
            snapshot: b"{\"doc\":1}".to_vec(),
            objects: BTreeMap::from([(kept, encode_object(4, &pixels))]),
            external: Some(ExternalObjects {
                document: alto_core::NodeId::new(),
                objects: ext.clone(),
            }),
            ..Default::default()
        };
        (parts, ext)
    }
    fn manifest_of(entries: &BTreeMap<String, Vec<u8>>) -> serde_json::Value {
        serde_json::from_slice(&entries["manifest.json"]).unwrap()
    }
    fn set_manifest(
        entries: &mut BTreeMap<String, Vec<u8>>,
        f: impl FnOnce(&mut serde_json::Value),
    ) {
        let mut m = manifest_of(entries);
        f(&mut m);
        entries.insert("manifest.json".into(), serde_json::to_vec(&m).unwrap());
    }
    #[test]
    fn thin_package_round_trips() {
        let (parts, ext) = thin_parts();
        let bytes = encode_parts(&parts).unwrap();
        let entries = read_entries(&bytes).unwrap();
        let m = manifest_of(&entries);
        assert_eq!(m["format"], 3);
        assert_eq!(m["external_history"]["objects"], 2);
        assert_eq!(entries[EXTERNAL_OBJECTS], external_objects_bytes(&ext));
        let p = decode_entries(entries).unwrap();
        let got = p.external.unwrap();
        assert_eq!(got.objects, ext);
        assert_eq!(got.document, parts.external.unwrap().document);
        assert_eq!(p.objects.len(), 1);
    }
    #[test]
    fn thin_package_with_no_versions_is_format_3() {
        let (mut parts, _) = thin_parts();
        parts.external.as_mut().unwrap().objects.clear();
        let entries = read_entries(&encode_parts(&parts).unwrap()).unwrap();
        assert_eq!(manifest_of(&entries)["format"], 3);
        assert_eq!(entries[EXTERNAL_OBJECTS], b"[]");
        let p = decode_entries(entries).unwrap();
        assert!(p.external.unwrap().objects.is_empty());
    }
    #[test]
    fn full_package_stays_format_2() {
        let (mut parts, _) = thin_parts();
        parts.external = None;
        let entries = package_entries(&parts).unwrap();
        let m = manifest_of(&entries);
        assert_eq!(m["format"], 2);
        assert!(m.get("external_history").is_none());
        assert!(!entries.contains_key(EXTERNAL_OBJECTS));
        assert!(decode_entries(entries).unwrap().external.is_none());
    }
    #[test]
    fn thin_package_rejects_bad_external_list() {
        let (parts, _) = thin_parts();
        let good = package_entries(&parts).unwrap();
        let corrupt = |entries: BTreeMap<String, Vec<u8>>| {
            assert!(
                matches!(decode_entries(entries), Err(StoreError::Corrupt(_))),
                "expected Corrupt"
            );
        };
        let (a, b) = (ContentHash::of(b"a"), ContentHash::of(b"b"));
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        // The list changed but the manifest still names the old digest.
        let mut e = good.clone();
        e.insert(EXTERNAL_OBJECTS.into(), serde_json::to_vec(&[lo]).unwrap());
        corrupt(e);
        // Count does not match.
        let mut e = good.clone();
        set_manifest(&mut e, |m| m["external_history"]["objects"] = 3.into());
        corrupt(e);
        // Not sorted, and duplicated: the digest and count are made to agree.
        for list in [vec![hi, lo], vec![lo, lo]] {
            let bytes = serde_json::to_vec(&list).unwrap();
            let mut e = good.clone();
            set_manifest(&mut e, |m| {
                m["external_history"]["digest"] = ContentHash::of(&bytes).to_string().into()
            });
            e.insert(EXTERNAL_OBJECTS.into(), bytes);
            corrupt(e);
        }
        // The list is missing.
        let mut e = good.clone();
        e.remove(EXTERNAL_OBJECTS);
        corrupt(e);
        // An object in the .alto is also listed as external.
        let kept = *parts.objects.keys().next().unwrap();
        let mut overlap = parts.clone();
        overlap.external.as_mut().unwrap().objects.insert(kept);
        corrupt(package_entries(&overlap).unwrap());
    }
    #[test]
    fn format_and_external_history_must_agree() {
        let (parts, _) = thin_parts();
        let thin = package_entries(&parts).unwrap();
        let unsupported = |entries: BTreeMap<String, Vec<u8>>| {
            assert!(matches!(
                decode_entries(entries),
                Err(StoreError::Unsupported(_))
            ));
        };
        let mut e = thin.clone();
        set_manifest(&mut e, |m| {
            m.as_object_mut().unwrap().remove("external_history");
        });
        unsupported(e);
        let mut e = thin.clone();
        set_manifest(&mut e, |m| m["format"] = 2.into());
        unsupported(e);
        let mut e = thin.clone();
        set_manifest(&mut e, |m| m["format"] = 4.into());
        unsupported(e);
        let mut e = thin;
        set_manifest(&mut e, |m| m["external_history"] = serde_json::Value::Null);
        unsupported(e);
    }
    #[test]
    fn thin_streaming_writer_matches_entries() {
        let (parts, _) = thin_parts();
        let expected = write_entries(&package_entries(&parts).unwrap()).unwrap();
        assert_eq!(encode_parts(&parts).unwrap(), expected);
        // One object is supplied through the fetch callback instead of `parts`.
        let mut held = parts.clone();
        let (h, b) = held.objects.pop_first().unwrap();
        let fetched = BTreeMap::from([(h, b.len() as u64)]);
        let bytes =
            encode_parts_fetching(&held, &fetched, |_| Ok::<_, StoreError>(b.clone())).unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(
            package_payload(&parts, &BTreeMap::new()).unwrap(),
            package_entries(&parts)
                .unwrap()
                .values()
                .map(|v| v.len() as u64)
                .sum::<u64>()
        );
    }
    #[test]
    fn a_folder_refuses_a_thin_package() {
        let (parts, _) = thin_parts();
        assert!(folder_entries(&parts).is_err());
    }
    #[test]
    fn fetching_writes_the_same_bytes_without_regrowing_the_buffer() {
        let parts = sample_parts();
        let expected = write_entries(&package_entries(&parts).unwrap()).unwrap();
        // Half the objects are fetched, the rest are in `parts`.
        let mut held = parts.clone();
        let fetched: BTreeMap<_, _> = parts
            .objects
            .iter()
            .step_by(2)
            .map(|(h, b)| {
                held.objects.remove(h);
                (*h, b.len() as u64)
            })
            .collect();
        let mut calls = 0;
        let bytes = encode_parts_fetching(&held, &fetched, |h| {
            calls += 1;
            Ok::<_, StoreError>(parts.objects[h].clone())
        })
        .unwrap();
        assert_eq!(calls, fetched.len());
        assert_eq!(bytes, expected);
        assert_eq!(encode_parts(&parts).unwrap(), expected);
        let entries = package_entries(&parts).unwrap();
        let capacity = entries.values().map(Vec::len).sum::<usize>()
            + entries
                .keys()
                .map(|n| ZIP_ENTRY_OVERHEAD + 2 * n.len())
                .sum::<usize>()
            + ZIP_END_OVERHEAD;
        assert!(bytes.len() <= capacity, "{} > {capacity}", bytes.len());
    }
    #[test]
    fn fetching_refuses_a_package_over_the_cap_before_fetching() {
        let parts = sample_parts();
        let fetched: BTreeMap<_, _> = [(ContentHash::of(b"big"), MAX_PACKAGE_BYTES)].into();
        let err = encode_parts_fetching(&parts, &fetched, |_| -> Result<Vec<u8>> {
            panic!("fetched over the cap")
        })
        .unwrap_err();
        assert!(err.to_string().contains("RESOURCE_LIMIT"), "{err}");
    }
    #[test]
    fn fetching_refuses_an_object_whose_size_changed() {
        let parts = sample_parts();
        let (h, b) = parts.objects.iter().next().unwrap();
        let mut held = parts.clone();
        held.objects.remove(h);
        let fetched: BTreeMap<_, _> = [(*h, b.len() as u64 + 1)].into();
        let err = encode_parts_fetching(&held, &fetched, |h| {
            Ok::<_, StoreError>(parts.objects[h].clone())
        })
        .unwrap_err();
        assert!(err.to_string().contains("changed size"), "{err}");
    }
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
            refs: VersionRefs::default(),
            external: None,
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
        // A damaged manifest must not discard branch/tag roots through implicit recovery.
        std::fs::write(dir.join("manifest.json"), b"{").unwrap();
        assert!(decode_entries(folder::read(&dir).unwrap()).is_err());
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
            refs: VersionRefs::default(),
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
    fn recovery_decoder_checks_object_paths_and_opaque_index() {
        let pixels = vec![255; 262144];
        let h = object_hash(4, &pixels);
        let parts = PackageParts {
            snapshot: b"{\"nodes\":[]}".to_vec(),
            objects: BTreeMap::from([(h, encode_object(4, &pixels))]),
            ..Default::default()
        };
        let entries = package_entries(&parts).unwrap();
        let opaque = std::collections::BTreeSet::from([h]);
        let package = decode_recovery_entries(entries.clone(), &opaque).unwrap();
        assert!(package.opaque.contains(&h));
        let mut wrong_path = entries.clone();
        let object = wrong_path.remove(&object_path(&h)).unwrap();
        wrong_path.insert(format!("objects/ff/{h}"), object);
        assert!(decode_recovery_entries(wrong_path, &opaque).is_err());
        let missing = std::collections::BTreeSet::from([ContentHash::of(b"missing")]);
        assert!(decode_recovery_entries(entries, &missing).is_err());
    }
    #[test]
    fn corrupt_object_is_rejected_without_implicit_repair() {
        let mut e = BTreeMap::new();
        let snapshot = b"{}";
        let h = ContentHash::of(snapshot);
        e.insert(format!("snapshots/{h}.json"), snapshot.to_vec());
        let bad = ContentHash::of(b"bad");
        e.insert(
            format!("objects/{}/{bad}", &bad.hex()[..2]),
            b"damaged".to_vec(),
        );
        e.insert("manifest.json".into(), manifest_bytes(h, None));
        assert!(decode_package(&write_entries(&e).unwrap()).is_err());
    }
    #[test]
    fn read_limited_stops_before_buffering_the_cap() {
        let mut reader = std::io::repeat(1u8).take(10_000);
        let err = read_limited(&mut reader, 100).unwrap_err();
        assert!(err.to_string().contains("512 MiB"), "{err}");
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod mirror_tests {
    use super::*;
    #[test]
    fn a_mirror_failure_does_not_undo_a_published_manifest() {
        let dir = std::env::temp_dir().join(format!("alto-mirror-{}", alto_core::NodeId::new().0));
        std::fs::create_dir_all(dir.join("document.json")).unwrap();
        let parts = PackageParts {
            snapshot: b"{}".to_vec(),
            ..Default::default()
        };
        let report = folder::write(&dir, &folder_entries(&parts).unwrap(), None).unwrap();
        assert_eq!(report.warnings.len(), 1);
        let opened = decode_entries(folder::read(&dir).unwrap()).unwrap();
        assert_eq!(opened.snapshot, parts.snapshot);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
