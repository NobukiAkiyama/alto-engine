//! How much saving a `.alto` allocates beyond the objects the engine already holds,
//! for a document whose objects do not compress, with one committed version.
//! `cargo run --release -p alto-engine --example save_bench -- <tiles>`.
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
        changes.push(TileChange {
            key: TileKey {
                tx: tx as i32,
                ty: ty as i32,
            },
            before: None,
            after: Some(after),
        });
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
    e.transact(d, "paint", vec![Op::SetTiles { layer, changes }], revision)
        .unwrap();
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let tiles: u32 = args.get(1).map(|s| s.parse().unwrap()).unwrap_or(400);
    let side = 64u32;
    let mut e = Engine::new();
    let d = e
        .create_document(DocumentMeta::srgb_u8(side * 256, side * 256))
        .unwrap();
    let node = Node::raster("layer");
    let layer = node.id;
    let parent = e.document(d).unwrap().root;
    let revision = e.document(d).unwrap().revision;
    e.transact(
        d,
        "layer",
        vec![Op::InsertNode {
            node,
            parent,
            index: 0,
        }],
        revision,
    )
    .unwrap();
    let keys: Vec<_> = (0..tiles).map(|i| (i % side, i / side)).collect();
    paint(&mut e, d, layer, &keys, 1);
    e.commit(d, "v1", 0, None, false).unwrap();
    // Half the tiles are redrawn after the commit, so the version keeps the old ones.
    paint(&mut e, d, layer, &keys[..keys.len() / 2], 5000);
    for round in 0..3u32 {
        let base = CUR.load(Relaxed);
        PEAK.store(base, Relaxed);
        let bytes = e.save_package(d).unwrap();
        let peak = PEAK.load(Relaxed) - base;
        println!(
            "tiles={tiles} round={round} alto={:.1}MiB peak={:.1}MiB ratio={:.2}",
            bytes.len() as f64 / MIB,
            peak as f64 / MIB,
            peak as f64 / bytes.len() as f64
        );
        drop(bytes);
    }
}
