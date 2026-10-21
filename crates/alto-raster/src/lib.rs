use alto_core::{ContentHash, PixelFormat, TILE, TileKey};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
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
    WrongLength { expected: usize, got: usize },
}

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

pub trait TileStore {
    fn get(&self, hash: &ContentHash) -> Option<Arc<TileData>>;
    fn get_checked(&self, hash: &ContentHash) -> Result<Option<Arc<TileData>>, String> {
        Ok(self.get(hash))
    }
    fn put(&mut self, data: TileData) -> ContentHash;
    fn is_opaque(&self, _: &ContentHash) -> bool {
        false
    }
}

struct StoredTile {
    format: PixelFormat,
    compressed: Vec<u8>,
    /// A digest of these exact compressed bytes detects later storage damage
    /// without rehashing 256 KiB on every cache miss. Recovery tiles also
    /// check their decoded content address on first use.
    compressed_hash: ContentHash,
    /// A journal recovery verifies the package digest first, then verifies
    /// each decoded tile against its content address on first use.
    verified: Cell<bool>,
    opaque: bool,
}
struct CpuCache {
    tiles: HashMap<ContentHash, (Arc<TileData>, u64)>,
    /// Clock -> hash, so eviction does not scan the whole map.
    order: BTreeMap<u64, ContentHash>,
    bytes: usize,
    clock: u64,
}
pub struct MemoryTileStore {
    tiles: HashMap<ContentHash, StoredTile>,
    /// Reachable tiles parked after a package exists. Not part of the working set.
    cold: RefCell<HashMap<ContentHash, StoredTile>>,
    fault: RefCell<Option<String>>,
    cache: RefCell<CpuCache>,
    budget: usize,
}
impl Default for MemoryTileStore {
    fn default() -> Self {
        Self {
            tiles: HashMap::new(),
            cold: RefCell::new(HashMap::new()),
            fault: RefCell::new(None),
            cache: RefCell::new(CpuCache {
                tiles: HashMap::new(),
                order: BTreeMap::new(),
                bytes: 0,
                clock: 0,
            }),
            budget: 128 * 1024 * 1024,
        }
    }
}
impl MemoryTileStore {
    pub fn retain(&mut self, keep: &BTreeSet<ContentHash>) {
        self.tiles.retain(|h, _| keep.contains(h));
        self.cold.borrow_mut().retain(|h, _| keep.contains(h));
        let c = self.cache.get_mut();
        c.tiles.retain(|h, _| keep.contains(h));
        c.order.retain(|_, h| keep.contains(h));
        c.bytes = c.tiles.values().map(|(d, _)| d.pixels.len()).sum();
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
            .borrow()
            .keys()
            .copied()
            .filter(|h| resident.contains(h))
            .collect();
        for hash in parked {
            if let Some(tile) = self.cold.borrow_mut().remove(&hash) {
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
                self.cold.borrow_mut().insert(hash, tile);
            }
        }
        self.cold
            .borrow_mut()
            .retain(|h, _| reachable.contains(h) && !resident.contains(h));
        let c = self.cache.get_mut();
        c.tiles.retain(|h, _| resident.contains(h));
        c.order.retain(|_, h| resident.contains(h));
        c.bytes = c.tiles.values().map(|(d, _)| d.pixels.len()).sum();
    }
    pub fn reachable(&self, hash: &ContentHash) -> bool {
        self.tiles.contains_key(hash) || self.cold.borrow().contains_key(hash)
    }
    pub fn take_fault(&self) -> Option<String> {
        self.fault.borrow_mut().take()
    }
    pub fn tamper_cold(&mut self, hash: &ContentHash) -> bool {
        let mut cold = self.cold.borrow_mut();
        let Some(tile) = cold.get_mut(hash) else {
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
        self.insert_stored(h, format, compressed, opaque, true);
    }
    pub fn insert_deferred(
        &mut self,
        h: ContentHash,
        format: PixelFormat,
        compressed: Vec<u8>,
        opaque: bool,
    ) {
        self.insert_stored(h, format, compressed, opaque, false);
    }
    fn insert_stored(
        &mut self,
        h: ContentHash,
        format: PixelFormat,
        compressed: Vec<u8>,
        opaque: bool,
        verified: bool,
    ) {
        self.cold.borrow_mut().remove(&h);
        let compressed_hash = ContentHash::of(&compressed);
        self.tiles.insert(
            h,
            StoredTile {
                format,
                compressed,
                compressed_hash,
                verified: Cell::new(verified),
                opaque,
            },
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
        self.tiles
            .values()
            .map(|t| t.compressed.len())
            .sum::<usize>()
            + self.cache.borrow().bytes
    }
    fn cache(&self, h: ContentHash, data: Arc<TileData>) {
        let mut c = self.cache.borrow_mut();
        c.clock += 1;
        let clock = c.clock;
        if !c.tiles.contains_key(&h) {
            c.bytes += data.pixels.len();
            c.tiles.insert(h, (data, clock));
            c.order.insert(clock, h);
        }
        while c.bytes > self.budget && c.tiles.len() > 1 {
            let Some((&tick, &old)) = c.order.iter().find(|(_, key)| **key != h) else {
                break;
            };
            c.order.remove(&tick);
            if let Some((d, _)) = c.tiles.remove(&old) {
                c.bytes -= d.pixels.len();
            } else {
                break;
            }
        }
    }
}
fn decode_stored(h: &ContentHash, stored: &StoredTile) -> Option<Arc<TileData>> {
    if ContentHash::of(&stored.compressed) != stored.compressed_hash {
        return None;
    }
    let pixels = lz4_flex::decompress_size_prepended(&stored.compressed).ok()?;
    let tile = Arc::new(TileData::new(stored.format, pixels).ok()?);
    if !stored.verified.get() {
        if tile.hash() != *h {
            return None;
        }
        stored.verified.set(true);
    }
    Some(tile)
}
impl TileStore for MemoryTileStore {
    fn get_checked(&self, h: &ContentHash) -> Result<Option<Arc<TileData>>, String> {
        let tile = self.get(h);
        if let Some(error) = self.take_fault() {
            Err(format!("CORRUPT_DATA: {error}"))
        } else {
            Ok(tile)
        }
    }
    fn is_opaque(&self, h: &ContentHash) -> bool {
        self.tiles.get(h).is_some_and(|t| t.opaque)
            || self.cold.borrow().get(h).is_some_and(|t| t.opaque)
    }
    fn get(&self, h: &ContentHash) -> Option<Arc<TileData>> {
        {
            let mut c = self.cache.borrow_mut();
            c.clock += 1;
            let clock = c.clock;
            let hit = c.tiles.get_mut(h).map(|(data, used)| {
                let old = *used;
                *used = clock;
                (data.clone(), old)
            });
            if let Some((data, old)) = hit {
                c.order.remove(&old);
                c.order.insert(clock, *h);
                return Some(data);
            }
        }
        if let Some(stored) = self.tiles.get(h) {
            let Some(tile) = decode_stored(h, stored) else {
                *self.fault.borrow_mut() = Some(format!("corrupt object {h}"));
                return None;
            };
            self.cache(*h, tile.clone());
            return Some(tile);
        }
        let decoded = {
            let cold = self.cold.borrow();
            let stored = cold.get(h)?;
            decode_stored(h, stored)
        };
        match decoded {
            Some(tile) => {
                self.cache(*h, tile.clone());
                Some(tile)
            }
            None => {
                *self.fault.borrow_mut() = Some(format!("corrupt object {h}"));
                None
            }
        }
    }
    fn put(&mut self, data: TileData) -> ContentHash {
        let h = data.hash();
        self.cold.borrow_mut().remove(&h);
        if let std::collections::hash_map::Entry::Vacant(entry) = self.tiles.entry(h) {
            let compressed = lz4_flex::compress_prepend_size(&data.pixels);
            let compressed_hash = ContentHash::of(&compressed);
            entry.insert(StoredTile {
                format: data.format,
                compressed,
                compressed_hash,
                verified: Cell::new(true),
                opaque: data.format == PixelFormat::Rgba8
                    && data.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255),
            });
            self.cache(h, Arc::new(data));
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn content_is_deduplicated_and_transparent_detected() {
        let pixels = vec![0; (TILE * TILE * 4) as usize];
        let tile = TileData::new(PixelFormat::Rgba8, pixels.clone()).unwrap();
        assert!(tile.is_empty());
        let mut store = MemoryTileStore::default();
        let a = store.put(tile);
        let b = store.put(TileData::new(PixelFormat::Rgba8, pixels).unwrap());
        assert_eq!(a, b);
        assert_eq!(store.len(), 1);
        assert!(store.get(&a).is_some());
    }
    #[test]
    fn deferred_tile_is_verified_before_it_enters_the_cpu_cache() {
        let pixels = vec![7; (TILE * TILE * 4) as usize];
        let compressed = lz4_flex::compress_prepend_size(&pixels);
        let wrong = ContentHash::of(b"wrong object name");
        let mut store = MemoryTileStore::default();
        store.insert_deferred(wrong, PixelFormat::Rgba8, compressed, true);
        assert!(
            store
                .get_checked(&wrong)
                .unwrap_err()
                .contains("CORRUPT_DATA")
        );
        assert!(!store.cache.borrow().tiles.contains_key(&wrong));
    }
    #[test]
    fn compressed_damage_is_detected_after_a_cpu_cache_eviction() {
        let mut store = MemoryTileStore::default();
        let tile = TileData::new(PixelFormat::Rgba8, vec![7; (TILE * TILE * 4) as usize]).unwrap();
        let hash = store.put(tile);
        store.cache.borrow_mut().tiles.clear();
        store.cache.borrow_mut().order.clear();
        store.cache.borrow_mut().bytes = 0;
        store.separate(&BTreeSet::new(), &BTreeSet::from([hash]));
        assert!(store.tamper_cold(&hash));
        assert!(
            store
                .get_checked(&hash)
                .unwrap_err()
                .contains("CORRUPT_DATA")
        );
    }
}

mod brush;
pub use brush::*;
