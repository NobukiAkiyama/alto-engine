//! How much a version commit writes to the journal and allocates, for a document
//! whose objects do not compress. `cargo run --release -p alto-engine --example
//! commit_bench -- <tiles> <mode>`, mode `full` (the record as main writes it) or
//! `incremental`.
use alto_core::{NodeId, TileKey};
use alto_doc::{DocumentMeta, Node, Op, TileChange};
use alto_engine::Engine;
use alto_raster::TileData;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
struct Count;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        PEAK.fetch_max(CUR.fetch_add(l.size(), Relaxed) + l.size(), Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        CUR.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        PEAK.fetch_max(CUR.fetch_add(n, Relaxed) + n, Relaxed);
        let r = unsafe { System.realloc(p, l, n) };
        CUR.fetch_sub(l.size(), Relaxed);
        r
    }
}
#[global_allocator]
static A: Count = Count;
const MIB: f64 = 1048576.0;

fn noise(seed: u32) -> TileData {
    let mut x = seed.wrapping_mul(2654435761) | 1;
    let mut v = vec![0u8; 262144];
    for b in v.chunks_mut(4) {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        b.copy_from_slice(&(x | 0xff00_0000).to_le_bytes());
    }
    TileData::new(alto_core::PixelFormat::Rgba8, v).unwrap()
}
fn paint(e: &mut Engine, d: alto_engine::DocHandle, layer: NodeId, keys: &[(u32, u32)], seed: u32) {
    let mut changes = Vec::new();
    for (i, &(tx, ty)) in keys.iter().enumerate() {
        let after = e.put_tile(noise(seed + i as u32)).unwrap();
        changes.push(TileChange { key: TileKey { tx: tx as i32, ty: ty as i32 }, before: None, after: Some(after) });
    }
    let doc = e.document(d).unwrap();
    let before_tiles = match &doc.node(layer).unwrap().kind {
        alto_doc::NodeKind::Raster { tiles, .. } => tiles.tiles.clone(),
        _ => panic!(),
    };
    for c in &mut changes {
        c.before = before_tiles.get(&c.key).copied();
    }
    let revision = doc.revision;
    e.transact(d, "paint", vec![Op::SetTiles { layer, changes }], revision).unwrap();
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tiles: u32 = args.get(1).map(|s| s.parse().unwrap()).unwrap_or(400);
    let mode = args.get(2).cloned().unwrap_or("full".into());
    let side = 64u32;
    let mut e = Engine::new();
    let d = e.create_document(DocumentMeta::srgb_u8(side * 256, side * 256)).unwrap();
    let node = Node::raster("layer");
    let layer = node.id;
    let parent = e.document(d).unwrap().root;
    let revision = e.document(d).unwrap().revision;
    e.transact(d, "layer", vec![Op::InsertNode { node, parent, index: 0 }], revision).unwrap();
    let keys: Vec<_> = (0..tiles).map(|i| (i % side, i / side)).collect();
    paint(&mut e, d, layer, &keys, 1);
    for round in 0..3u32 {
        // One tile changes between commits, as a light edit would.
        paint(&mut e, d, layer, &keys[..1], 1000 + round);
        let before = e.capture_version_state(d).unwrap();
        let base = CUR.load(Relaxed);
        PEAK.store(base, Relaxed);
        e.commit(d, "bench", round as i64, None, false).unwrap();
        let record = match mode.as_str() {
            "full" => e.version_record(d, before.digest).unwrap(),
            "incremental" => e.version_record_since(d, &before).unwrap(),
            m => panic!("mode {m}"),
        };
        let peak = PEAK.load(Relaxed) - base;
        e.release_version_state(before);
        println!(
            "{mode} tiles={tiles} round={round} record={:.1}MiB peak={:.1}MiB",
            record.len() as f64 / MIB,
            peak as f64 / MIB
        );
    }
}
