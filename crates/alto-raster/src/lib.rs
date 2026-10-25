use alto_core::{ContentHash, PixelFormat, TILE, TileKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileMap {
    pub format: PixelFormat,
    pub tiles: BTreeMap<TileKey, ContentHash>,
}
impl TileMap {
    pub fn new(format: PixelFormat) -> Self {
        Self {
            format,
            tiles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TileError {
    WrongLength {
        expected: usize,
        got: usize,
    },
    /// Stored bytes no longer match their digest or content address.
    Corrupt(ContentHash),
}
impl std::fmt::Display for TileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongLength { expected, got } => {
                write!(f, "CORRUPT_DATA: tile length {got}, expected {expected}")
            }
            Self::Corrupt(hash) => write!(f, "CORRUPT_DATA: corrupt object {hash}"),
        }
    }
}
impl std::error::Error for TileError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileData {
    pub format: PixelFormat,
    pub pixels: Box<[u8]>,
}

impl TileData {
    pub fn new(format: PixelFormat, pixels: Vec<u8>) -> Result<Self, TileError> {
        let expected = (TILE * TILE) as usize * format.bytes_per_pixel();
        if pixels.len() != expected {
            return Err(TileError::WrongLength {
                expected,
                got: pixels.len(),
            });
        }
        Ok(Self {
            format,
            pixels: pixels.into_boxed_slice(),
        })
    }
    pub fn hash(&self) -> ContentHash {
        ContentHash::of_tagged(
            match self.format {
                PixelFormat::Rgba8 => 4,
                PixelFormat::R8 => 1,
            },
            &self.pixels,
        )
    }
    pub fn is_empty(&self) -> bool {
        match self.format {
            PixelFormat::Rgba8 => self
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 0),
            PixelFormat::R8 => self.pixels.iter().all(|alpha| *alpha == 0),
        }
    }
}

/// Where tiles live. Reading never changes the store: decoding happens in the caller,
/// which may cache the result in a `DecodeCache` it owns.
pub trait TileStore {
    /// The compressed object, or `None` if the store does not hold it.
    fn object(&self, hash: &ContentHash) -> Option<Arc<StoredTile>>;
    fn is_opaque(&self, _: &ContentHash) -> bool {
        false
    }
}

/// Decodes a tile without caching. `Ok(None)` means the store does not hold it.
pub fn load(store: &dyn TileStore, hash: &ContentHash) -> Result<Option<Arc<TileData>>, TileError> {
    store
        .object(hash)
        .map(|o| o.decode(hash).map(Arc::new))
        .transpose()
}

/// One compressed tile. Shared between threads: decoding only reads it, and the
/// first-use verification flag is atomic.
pub struct StoredTile {
    pub format: PixelFormat,
    compressed: Vec<u8>,
    /// A digest of these exact compressed bytes detects later storage damage
    /// without rehashing 256 KiB on every decode. Recovery tiles also
    /// check their decoded content address on first use.
    compressed_hash: ContentHash,
    /// A journal recovery verifies the package digest first, then verifies
    /// each decoded tile against its content address on first use.
    verified: AtomicBool,
    pub opaque: bool,
}
impl StoredTile {
    fn new(format: PixelFormat, compressed: Vec<u8>, opaque: bool, verified: bool) -> Self {
        Self {
            format,
            compressed_hash: ContentHash::of(&compressed),
            compressed,
            verified: AtomicBool::new(verified),
            opaque,
        }
    }
    /// The compressed bytes for a decoder elsewhere, after checking their digest, and
    /// whether that decoder must also check the content address (first use).
    pub fn for_decoder(&self, hash: &ContentHash) -> Result<(&[u8], bool), TileError> {
        if ContentHash::of(&self.compressed) != self.compressed_hash {
            return Err(TileError::Corrupt(*hash));
        }
        Ok((&self.compressed, !self.verified.load(Ordering::Relaxed)))
    }
    /// A decoder elsewhere checked the content address.
    pub fn mark_verified(&self) {
        self.verified.store(true, Ordering::Relaxed);
    }
    /// The package object bytes (`ALTOOBJ1`, tag, LZ4 size-prepended pixels) without
    /// decoding: the stored compression is that format. A tile never checked against
    /// its content address is checked once first, so damage is not copied onward.
    pub fn encoded(&self, hash: &ContentHash) -> Result<Vec<u8>, TileError> {
        if ContentHash::of(&self.compressed) != self.compressed_hash {
            return Err(TileError::Corrupt(*hash));
        }
        if !self.verified.load(Ordering::Relaxed) {
            self.decode(hash)?;
        }
        let mut out = Vec::with_capacity(9 + self.compressed.len());
        out.extend_from_slice(b"ALTOOBJ1");
        out.push(match self.format {
            PixelFormat::Rgba8 => 4,
            PixelFormat::R8 => 1,
        });
        out.extend_from_slice(&self.compressed);
        Ok(out)
    }
    pub fn decode(&self, hash: &ContentHash) -> Result<TileData, TileError> {
        let corrupt = || TileError::Corrupt(*hash);
        if ContentHash::of(&self.compressed) != self.compressed_hash {
            return Err(corrupt());
        }
        let pixels =
            lz4_flex::decompress_size_prepended(&self.compressed).map_err(|_| corrupt())?;
        let tile = TileData::new(self.format, pixels).map_err(|_| corrupt())?;
        if !self.verified.load(Ordering::Relaxed) {
            if tile.hash() != *hash {
                return Err(corrupt());
            }
            self.verified.store(true, Ordering::Relaxed);
        }
        Ok(tile)
    }
}

/// Compressed tiles by content hash.
#[derive(Default)]
pub struct ObjectStore {
    tiles: HashMap<ContentHash, Arc<StoredTile>>,
    /// Reachable tiles parked after a package exists. Not part of the working set.
    cold: HashMap<ContentHash, Arc<StoredTile>>,
}
impl ObjectStore {
    pub fn retain(&mut self, keep: &BTreeSet<ContentHash>) {
        self.tiles.retain(|h, _| keep.contains(h));
        self.cold.retain(|h, _| keep.contains(h));
    }
    /// Moves tiles outside `resident` into cold storage when they are still reachable.
    /// Recalls resident tiles that were parked. Drops everything else.
    pub fn separate(
        &mut self,
        resident: &BTreeSet<ContentHash>,
        reachable: &BTreeSet<ContentHash>,
    ) {
        let parked: Vec<ContentHash> = self
            .cold
            .keys()
            .copied()
            .filter(|h| resident.contains(h))
            .collect();
        for hash in parked {
            if let Some(tile) = self.cold.remove(&hash) {
                self.tiles.entry(hash).or_insert(tile);
            }
        }
        let evict: Vec<ContentHash> = self
            .tiles
            .keys()
            .copied()
            .filter(|h| !resident.contains(h))
            .collect();
        for hash in evict {
            let Some(tile) = self.tiles.remove(&hash) else {
                continue;
            };
            if reachable.contains(&hash) {
                self.cold.insert(hash, tile);
            }
        }
        self.cold
            .retain(|h, _| reachable.contains(h) && !resident.contains(h));
    }
    pub fn reachable(&self, hash: &ContentHash) -> bool {
        self.tiles.contains_key(hash) || self.cold.contains_key(hash)
    }
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn tamper_cold(&mut self, hash: &ContentHash) -> bool {
        let Some(tile) = self.cold.get_mut(hash).and_then(Arc::get_mut) else {
            return false;
        };
        if let Some(byte) = tile.compressed.last_mut() {
            *byte ^= 0xff;
            true
        } else {
            false
        }
    }
    pub fn insert_verified(
        &mut self,
        h: ContentHash,
        format: PixelFormat,
        compressed: Vec<u8>,
        opaque: bool,
    ) {
        self.cold.remove(&h);
        self.tiles.insert(
            h,
            Arc::new(StoredTile::new(format, compressed, opaque, true)),
        );
    }
    pub fn insert_deferred(
        &mut self,
        h: ContentHash,
        format: PixelFormat,
        compressed: Vec<u8>,
        opaque: bool,
    ) {
        self.cold.remove(&h);
        self.tiles.insert(
            h,
            Arc::new(StoredTile::new(format, compressed, opaque, false)),
        );
    }
    pub fn len(&self) -> usize {
        self.tiles.len()
    }
    pub fn contains(&self, h: &ContentHash) -> bool {
        self.tiles.contains_key(h)
    }
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
    pub fn allocated_bytes(&self) -> usize {
        self.tiles.values().map(|t| t.compressed.len()).sum()
    }
}
impl TileStore for ObjectStore {
    fn object(&self, h: &ContentHash) -> Option<Arc<StoredTile>> {
        self.tiles.get(h).or_else(|| self.cold.get(h)).cloned()
    }
    fn is_opaque(&self, h: &ContentHash) -> bool {
        self.object(h).is_some_and(|t| t.opaque)
    }
}
impl ObjectStore {
    pub fn insert_tile(&mut self, data: &TileData) -> ContentHash {
        let h = data.hash();
        if let Some(parked) = self.cold.remove(&h) {
            self.tiles.insert(h, parked);
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = self.tiles.entry(h) {
            let opaque = data.format == PixelFormat::Rgba8
                && data.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
            let compressed = lz4_flex::compress_prepend_size(&data.pixels);
            entry.insert(Arc::new(StoredTile::new(
                data.format,
                compressed,
                opaque,
                true,
            )));
        }
        h
    }
}

/// Decoded tiles, least recently used first out, within a byte budget.
pub struct DecodeCache {
    tiles: HashMap<ContentHash, (Arc<TileData>, u64)>,
    /// Clock -> hash, so eviction does not scan the whole map.
    order: BTreeMap<u64, ContentHash>,
    bytes: usize,
    clock: u64,
    budget: usize,
}
impl DecodeCache {
    pub fn new(budget: usize) -> Self {
        Self {
            tiles: HashMap::new(),
            order: BTreeMap::new(),
            bytes: 0,
            clock: 0,
            budget,
        }
    }
    pub fn get(
        &mut self,
        store: &dyn TileStore,
        h: &ContentHash,
    ) -> Result<Option<Arc<TileData>>, TileError> {
        self.clock += 1;
        if let Some((data, used)) = self.tiles.get_mut(h) {
            let data = data.clone();
            self.order.remove(used);
            *used = self.clock;
            self.order.insert(self.clock, *h);
            return Ok(Some(data));
        }
        let Some(tile) = load(store, h)? else {
            return Ok(None);
        };
        self.insert(*h, tile.clone());
        Ok(Some(tile))
    }
    pub fn insert(&mut self, h: ContentHash, data: Arc<TileData>) {
        self.clock += 1;
        if self.tiles.contains_key(&h) {
            return;
        }
        self.bytes += data.pixels.len();
        self.tiles.insert(h, (data, self.clock));
        self.order.insert(self.clock, h);
        while self.bytes > self.budget && self.tiles.len() > 1 {
            let Some((&tick, &old)) = self.order.iter().find(|(_, key)| **key != h) else {
                break;
            };
            self.order.remove(&tick);
            if let Some((d, _)) = self.tiles.remove(&old) {
                self.bytes -= d.pixels.len();
            }
        }
    }
    pub fn retain(&mut self, keep: impl Fn(&ContentHash) -> bool) {
        self.tiles.retain(|h, _| keep(h));
        self.order.retain(|_, h| keep(h));
        self.bytes = self.tiles.values().map(|(d, _)| d.pixels.len()).sum();
    }
    pub fn clear(&mut self) {
        self.retain(|_| false);
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn contains(&self, h: &ContentHash) -> bool {
        self.tiles.contains_key(h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tile(v: u8) -> TileData {
        TileData::new(PixelFormat::Rgba8, vec![v; (TILE * TILE * 4) as usize]).unwrap()
    }
    #[test]
    fn content_is_deduplicated_and_transparent_detected() {
        let pixels = vec![0; (TILE * TILE * 4) as usize];
        let empty = TileData::new(PixelFormat::Rgba8, pixels.clone()).unwrap();
        assert!(empty.is_empty());
        let mut store = ObjectStore::default();
        let a = store.insert_tile(&empty);
        let b = store.insert_tile(&TileData::new(PixelFormat::Rgba8, pixels).unwrap());
        assert_eq!(a, b);
        assert_eq!(store.len(), 1);
        assert!(load(&store, &a).unwrap().is_some());
    }
    #[test]
    fn deferred_tile_is_verified_before_it_enters_the_cpu_cache() {
        let pixels = vec![7; (TILE * TILE * 4) as usize];
        let compressed = lz4_flex::compress_prepend_size(&pixels);
        let wrong = ContentHash::of(b"wrong object name");
        let mut store = ObjectStore::default();
        store.insert_deferred(wrong, PixelFormat::Rgba8, compressed, true);
        let mut cache = DecodeCache::new(1 << 30);
        assert_eq!(cache.get(&store, &wrong), Err(TileError::Corrupt(wrong)));
        assert!(!cache.contains(&wrong));
    }
    #[test]
    fn compressed_damage_is_detected_after_a_cpu_cache_eviction() {
        let mut store = ObjectStore::default();
        let hash = store.insert_tile(&tile(7));
        store.separate(&BTreeSet::new(), &BTreeSet::from([hash]));
        assert!(store.tamper_cold(&hash));
        assert_eq!(load(&store, &hash), Err(TileError::Corrupt(hash)));
    }
    #[test]
    fn corruption_is_reported_only_by_the_read_that_found_it() {
        let mut store = ObjectStore::default();
        let bad = store.insert_tile(&tile(7));
        let good = store.insert_tile(&tile(9));
        store.separate(&BTreeSet::from([good]), &BTreeSet::from([bad, good]));
        assert!(store.tamper_cold(&bad));
        let mut cache = DecodeCache::new(1 << 30);
        assert_eq!(cache.get(&store, &bad), Err(TileError::Corrupt(bad)));
        assert!(cache.get(&store, &good).unwrap().is_some());
        assert_eq!(cache.get(&store, &ContentHash::of(b"absent")), Ok(None));
    }
    #[test]
    fn decode_cache_evicts_least_recently_used_within_its_budget() {
        let mut store = ObjectStore::default();
        let hashes: Vec<_> = (0..3).map(|v| store.insert_tile(&tile(v))).collect();
        let mut cache = DecodeCache::new(2 * 262144);
        cache.get(&store, &hashes[0]).unwrap();
        cache.get(&store, &hashes[1]).unwrap();
        cache.get(&store, &hashes[0]).unwrap();
        cache.get(&store, &hashes[2]).unwrap();
        assert!(cache.contains(&hashes[0]) && cache.contains(&hashes[2]));
        assert!(!cache.contains(&hashes[1]));
        assert_eq!(cache.bytes(), 2 * 262144);
    }
    #[test]
    fn encoded_objects_are_the_package_format_without_a_decode() {
        let mut store = ObjectStore::default();
        let t = tile(5);
        let hash = store.insert_tile(&t);
        let mut expected = b"ALTOOBJ1".to_vec();
        expected.push(4);
        expected.extend(lz4_flex::compress_prepend_size(&t.pixels));
        assert_eq!(
            store.object(&hash).unwrap().encoded(&hash).unwrap(),
            expected
        );
        store.separate(&BTreeSet::new(), &BTreeSet::from([hash]));
        assert!(store.tamper_cold(&hash));
        assert_eq!(
            store.object(&hash).unwrap().encoded(&hash),
            Err(TileError::Corrupt(hash))
        );
    }
    #[test]
    fn the_object_store_can_be_shared_between_threads() {
        fn shareable<T: Send + Sync>() {}
        shareable::<ObjectStore>();
        let mut store = ObjectStore::default();
        let hashes: Vec<_> = (0..8).map(|v| store.insert_tile(&tile(v))).collect();
        let decoded: Vec<_> = std::thread::scope(|s| {
            let store = &store;
            let workers: Vec<_> = hashes
                .iter()
                .map(|h| s.spawn(move || load(store, h).unwrap().unwrap()))
                .collect();
            workers.into_iter().map(|w| w.join().unwrap()).collect()
        });
        for (h, t) in hashes.iter().zip(decoded) {
            assert_eq!(t.hash(), *h);
        }
    }
}

mod brush;
pub use brush::*;
