#![cfg(target_arch = "wasm32")]
mod spike;
use alto_core::{Affine2, AssetId, ContentHash, NodeId, PixelFormat, TileKey};
use alto_doc::{AssetKind, AssetRecord, DocumentMeta, NodeKind, Op, TileChange};
use alto_engine::{DocHandle, Engine};
use alto_gpu::renderer::{Renderer, Viewport};
use alto_raster::{BrushConfig, StrokeSample, StrokeSampler, TileData, TileMap};
use alto_scene::RenderScene;
use serde::Deserialize;
use serde_json::json;
use wasm_bindgen::prelude::*;

fn error(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}
fn clock() -> f64 {
    js_sys::global()
        .unchecked_into::<web_sys::WorkerGlobalScope>()
        .performance()
        .map_or(0.0, |p| p.now())
}
#[derive(Deserialize)]
struct Request {
    intent: String,
    ops: Vec<Op>,
    #[serde(rename = "baseRevision")]
    base_revision: u64,
}
#[wasm_bindgen]
pub struct Runtime {
    engine: Engine,
    doc: DocHandle,
    renderer: Renderer,
    scene: RenderScene,
    canvas: web_sys::OffscreenCanvas,
    viewport: Viewport,
    sampler: Option<StrokeSampler>,
    layer: Option<NodeId>,
    mask: bool,
    input_inverse: [f64; 6],
    journal: Vec<Vec<u8>>,
    saved: ContentHash,
    budget: u32,
    warnings: Vec<String>,
}
#[wasm_bindgen]
impl Runtime {
    #[wasm_bindgen(js_name=protocolVersion)]
    pub fn protocol_version(&self) -> u32 {
        4
    }
    #[wasm_bindgen(js_name=create)]
    pub async fn create(canvas: web_sys::OffscreenCanvas, budget: u32) -> Result<Runtime, JsValue> {
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::OffscreenCanvas(canvas.clone()))
            .map_err(error)?;
        let mut renderer =
            Renderer::new(&instance, surface, canvas.width(), canvas.height(), budget)
                .await
                .map_err(error)?;
        renderer.set_clock(clock);
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(1024, 768))
            .map_err(error)?;
        let scene = engine.render_scene(doc).map_err(error)?;
        let saved = ContentHash::of(&engine.snapshot(doc).map_err(error)?);
        Ok(Self {
            engine,
            doc,
            renderer,
            scene,
            canvas,
            viewport: Viewport::default(),
            sampler: None,
            layer: None,
            mask: false,
            input_inverse: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            journal: vec![],
            saved,
            budget,
            warnings: vec![],
        })
    }
    #[wasm_bindgen(js_name=createDocument)]
    pub fn create_document(&mut self, w: u32, h: u32) -> Result<String, JsValue> {
        if w == 0 || h == 0 || w > 65536 || h > 65536 {
            return Err(error("INVALID_ARGUMENT: canvas must be 1..65536 pixels"));
        }
        self.renderer.cancel_stroke();
        let mut engine = Engine::new();
        let doc = engine
            .create_document(DocumentMeta::srgb_u8(w, h))
            .map_err(error)?;
        self.engine = engine;
        self.doc = doc;
        self.sampler = None;
        self.layer = None;
        self.journal.clear();
        self.saved = self.state_hash()?;
        self.viewport = Viewport {
            center: [w as f64 / 2.0, h as f64 / 2.0],
            zoom: 1.0,
            rotation: 0.0,
        };
        self.reload()?;
        self.view()
    }
    pub fn view(&self) -> Result<String, JsValue> {
        let mut value = self.envelope()?;
        let doc = self.engine.document(self.doc).map_err(error)?;
        value["document"] = serde_json::to_value(doc).map_err(error)?;
        serde_json::to_string(&value).map_err(error)
    }
    fn envelope(&self) -> Result<serde_json::Value, JsValue> {
        let doc = self.engine.document(self.doc).map_err(error)?;
        let (undo, redo) = self.engine.history_state(self.doc).map_err(error)?;
        let hash = ContentHash::of(&self.engine.snapshot(self.doc).map_err(error)?);
        let mut warnings = self.warnings.clone();
        warnings.extend(self.engine.font_warnings(self.doc).map_err(error)?);
        let versions = self.engine.version_status(self.doc).map_err(error)?;
        let state = self.state_hash()?;
        Ok(json!({
            "history": {"undo": undo, "redo": redo},
            "snapshotHash": hash,
            "stateHash": state,
            "dirty": state != self.saved,
            "warnings": warnings,
            "versions": versions,
            "revision": doc.revision,
        }))
    }
    /// Hot-path reply: changed nodes, removals, and document-level fields.
    /// Open, create, save, and mark-saved still use the full `view`.
    fn patch_view(&self, changes: &alto_doc::ChangeSet) -> Result<String, JsValue> {
        let doc = self.engine.document(self.doc).map_err(error)?;
        let mut nodes = serde_json::Map::new();
        let mut removed = Vec::new();
        for id in &changes.nodes {
            match doc.nodes.get(id) {
                Some(node) => {
                    nodes.insert(id.0.to_string(), serde_json::to_value(node).map_err(error)?);
                }
                None => removed.push(id.0.to_string()),
            }
        }
        let mut value = self.envelope()?;
        value["patch"] = json!(true);
        value["nodes"] = serde_json::Value::Object(nodes);
        value["removed"] = json!(removed);
        if changes.meta_changed {
            value["meta"] = serde_json::to_value(&doc.meta).map_err(error)?;
        }
        if changes.selection_changed {
            value["selection"] = serde_json::to_value(&doc.selection).map_err(error)?;
        }
        if changes.assets_changed {
            value["assets"] = serde_json::to_value(&doc.assets).map_err(error)?;
        }
        serde_json::to_string(&value).map_err(error)
    }
    pub fn transact(&mut self, request: &str) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: finish active tool first"));
        }
        let r: Request = serde_json::from_str(request).map_err(error)?;
        let edited = self
            .engine
            .transact(self.doc, &r.intent, r.ops.clone(), r.base_revision)
            .map_err(error)?;
        self.journal.push(
            self.engine
                .journal_record(&r.intent, &r.ops)
                .map_err(error)?,
        );
        self.refresh()?;
        self.patch_view(&edited)
    }
    pub fn undo(&mut self) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: finish active tool first"));
        }
        let ops = self.engine.undo_operations(self.doc).map_err(error)?;
        if ops.is_empty() {
            return self.view();
        }
        let edited = self
            .engine
            .undo(self.doc)
            .map_err(error)?
            .unwrap_or_default();
        self.journal
            .push(self.engine.journal_record("Undo", &ops).map_err(error)?);
        self.refresh()?;
        self.patch_view(&edited)
    }
    pub fn redo(&mut self) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: finish active tool first"));
        }
        let ops = self.engine.redo_operations(self.doc).map_err(error)?;
        if ops.is_empty() {
            return self.view();
        }
        let edited = self
            .engine
            .redo(self.doc)
            .map_err(error)?
            .unwrap_or_default();
        self.journal
            .push(self.engine.journal_record("Redo", &ops).map_err(error)?);
        self.refresh()?;
        self.patch_view(&edited)
    }
    pub fn resize(&mut self, w: u32, h: u32) {
        self.canvas.set_width(w.max(1));
        self.canvas.set_height(h.max(1));
        self.renderer.resize(w, h);
    }
    pub fn viewport(&mut self, x: f64, y: f64, zoom: f64, rotation: f64) -> Result<(), JsValue> {
        if ![x, y, zoom, rotation].iter().all(|n| n.is_finite()) || zoom <= 0.0 {
            return Err(error("INVALID_ARGUMENT: viewport"));
        }
        self.viewport = Viewport {
            center: [x, y],
            zoom: zoom.clamp(0.01, 128.0),
            rotation,
        };
        Ok(())
    }
    #[wasm_bindgen(js_name=setVisibleTileJobs)]
    pub fn set_visible_tile_jobs(&mut self, enabled: bool) {
        self.renderer.set_visible_tile_jobs(enabled);
    }
    #[wasm_bindgen(js_name=resetRenderCache)]
    pub fn reset_render_cache(&mut self, cpu: bool) {
        self.renderer.reset_render_cache();
        if cpu {
            self.engine.clear_decoded_tile_cache();
        }
    }
    pub fn tick(&mut self, budget_ms: f64) -> Result<String, JsValue> {
        let start = clock();
        let r = self
            .renderer
            .render(&self.scene, &self.engine, self.viewport, budget_ms)
            .map_err(error)?;
        let profile = r.profile.map(|p| {
            json!({
                "composeMs": p.compose_ms, "commandsMs": p.commands_ms,
                "getMs": p.get_ms, "writeMs": p.write_ms,
                "uploads": p.uploads, "atlasHits": p.atlas_hits,
                "uploadBytes": p.upload_bytes, "pyramidHits": p.pyramid_hits,
                "maxStepMs": p.max_step_ms, "steps": p.steps,
                "uniqueInputs": p.unique_inputs, "atlasEvictions": p.atlas_evictions,
            })
        });
        let prefetch = self.renderer.prefetch_status(&self.scene, self.viewport);
        Ok(json!({"composed":r.composed,"pending":r.pending,"gpuBytes":r.gpu_bytes,"cpuMs":clock()-start,"wasmBytes":wasm_bindgen::memory().unchecked_into::<js_sys::WebAssembly::Memory>().buffer().unchecked_into::<js_sys::ArrayBuffer>().byte_length(),"adapter":self.renderer.adapter_name,"profile":profile,"prefetch":{"candidates":prefetch.candidates,"ready":prefetch.ready,"composed":prefetch.composed,"totalMs":prefetch.total_ms,"maxStepMs":prefetch.max_step_ms,"maxPlanMs":prefetch.max_plan_ms,"maxInputMs":prefetch.max_input_ms,"maxGetMs":prefetch.max_get_ms,"maxConvertMs":prefetch.max_convert_ms,"maxWriteMs":prefetch.max_write_ms,"maxSubmitMs":prefetch.max_submit_ms,"cancelled":prefetch.cancelled,"uploadBytes":prefetch.upload_bytes}}).to_string())
    }
    #[wasm_bindgen(js_name=prefetchStep)]
    pub fn prefetch_step(&mut self) -> Result<String, JsValue> {
        let result = self
            .renderer
            .prefetch_step(&self.scene, &self.engine, self.viewport)
            .map_err(error)?;
        Ok(match result {
            alto_gpu::renderer::JobStep::Idle => "idle",
            alto_gpu::renderer::JobStep::Progress => "progress",
            alto_gpu::renderer::JobStep::Ready => "ready",
            alto_gpu::renderer::JobStep::Blocked => "blocked",
        }
        .into())
    }
    #[wasm_bindgen(js_name=setProfiling)]
    pub fn set_profiling(&mut self, enabled: bool) {
        self.renderer.set_profiling(enabled);
    }
    #[wasm_bindgen(js_name=beginBrush)]
    pub fn begin_brush(
        &mut self,
        layer: &str,
        config: &str,
        color: &[f32],
        erase: bool,
        mask: bool,
    ) -> Result<(), JsValue> {
        if self.sampler.is_some()
            || color.len() != 4
            || color.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0)
        {
            return Err(error("INVALID_ARGUMENT: brush state/color"));
        }
        let id: NodeId = serde_json::from_str(&format!("\"{layer}\"")).map_err(error)?;
        let mut cfg: BrushConfig = serde_json::from_str(config).map_err(error)?;
        if ![cfg.size, cfg.hardness, cfg.spacing, cfg.opacity, cfg.flow]
            .iter()
            .all(|v| v.is_finite())
            || cfg.size <= 0.0
            || cfg.size > 2048.0
        {
            return Err(error("INVALID_ARGUMENT: brush size"));
        }
        cfg.hardness = cfg.hardness.clamp(0.0, 0.999);
        cfg.spacing = cfg.spacing.clamp(0.01, 1.0);
        cfg.flow = cfg.flow.clamp(0.0, 1.0);
        cfg.opacity = cfg.opacity.clamp(0.0, 1.0);
        let doc = self.engine.document(self.doc).map_err(error)?;
        if (!mask && !matches!(doc.node(id).map_err(error)?.kind, NodeKind::Raster { .. }))
            || (mask && doc.node(id).map_err(error)?.props.mask.is_none())
        {
            return Err(error("INVALID_ARGUMENT: raster layer required"));
        }
        let mut current = Some(id);
        let mut chain = vec![];
        while let Some(n) = current {
            let node = doc.node(n).map_err(error)?;
            if node.props.locked {
                return Err(error("INVALID_ARGUMENT: layer is locked"));
            }
            chain.push(node.props.transform.0);
            current = node.parent;
        }
        let world = chain
            .into_iter()
            .rev()
            .fold(Affine2::default(), |a, m| a.concat(Affine2(m)))
            .0;
        let world = if mask {
            let props = &doc.node(id).map_err(error)?.props;
            if props.mask_linked {
                Affine2(world)
            } else {
                Affine2::default()
            }
            .concat(props.mask_transform)
            .0
        } else {
            world
        };
        self.mask = mask;
        self.input_inverse = Affine2(world)
            .inverse()
            .ok_or_else(|| error("INVALID_ARGUMENT: singular layer transform"))?
            .0;
        self.renderer
            .begin_stroke(
                id,
                if mask && erase {
                    [0.0, 0.0, 0.0, 1.0]
                } else {
                    [color[0], color[1], color[2], color[3]]
                },
                cfg.hardness,
                cfg.opacity,
                erase && !mask,
                mask,
                world,
            )
            .map_err(error)?;
        self.sampler = Some(StrokeSampler::new(cfg));
        self.layer = Some(id);
        Ok(())
    }
    #[wasm_bindgen(js_name=brushInput)]
    pub fn brush_input(&mut self, samples: &[f32]) -> Result<(), JsValue> {
        if !samples.len().is_multiple_of(6) {
            return Err(error("INVALID_ARGUMENT: sample layout"));
        }
        let sampler = self
            .sampler
            .as_mut()
            .ok_or_else(|| error("CONFLICT: no tool"))?;
        let mut dabs = Vec::new();
        for s in samples.as_chunks::<6>().0 {
            let p = Affine2(self.input_inverse).apply([s[0] as f64, s[1] as f64]);
            dabs.extend(sampler.input(StrokeSample {
                pos: [p[0] as f32, p[1] as f32],
                pressure: s[2],
                tilt: [s[3], s[4]],
                t_ms: s[5] as f64,
            }));
        }
        let layer = self.layer.ok_or_else(|| error("CONFLICT: no layer"))?;
        let doc = self.engine.document(self.doc).map_err(error)?;
        let node = doc.node(layer).map_err(error)?;
        let tiles = if self.mask {
            node.props
                .mask
                .as_ref()
                .ok_or_else(|| error("INVALID_ARGUMENT: no mask"))?
        } else {
            let NodeKind::Raster { tiles } = &node.kind else {
                return Err(error("INVALID_ARGUMENT: raster"));
            };
            tiles
        };
        let world = Affine2(self.input_inverse)
            .inverse()
            .ok_or_else(|| error("INVALID_ARGUMENT: singular transform"))?
            .0;
        self.renderer
            .stamp(
                &dabs,
                &tiles.tiles,
                &self.engine,
                doc.selection
                    .as_ref()
                    .map(|s| (s, world, [doc.meta.width, doc.meta.height])),
            )
            .map_err(error)?;
        Ok(())
    }
    #[wasm_bindgen(js_name=endBrush)]
    pub async fn end_brush(&mut self) -> Result<String, JsValue> {
        let layer = self.layer.ok_or_else(|| error("CONFLICT: no tool"))?;
        let results = self.renderer.finish_stroke().await.map_err(error)?;
        let mut changes = vec![];
        for (key, before, tile) in results {
            let after = self.engine.put_tile(tile);
            if before != after {
                changes.push(TileChange { key, before, after });
            }
        }
        self.sampler = None;
        self.layer = None;
        let edited = if !changes.is_empty() {
            let ops = if self.mask {
                let mut mask = self
                    .engine
                    .document(self.doc)
                    .map_err(error)?
                    .node(layer)
                    .map_err(error)?
                    .props
                    .mask
                    .clone()
                    .ok_or_else(|| error("INVALID_ARGUMENT: no mask"))?;
                for c in &changes {
                    if let Some(h) = c.after {
                        mask.tiles.insert(c.key, h);
                    } else {
                        mask.tiles.remove(&c.key);
                    }
                }
                vec![Op::SetMask {
                    layer,
                    mask: Some(mask),
                }]
            } else {
                vec![Op::SetTiles { layer, changes }]
            };
            let rev = self.engine.document(self.doc).map_err(error)?.revision;
            let edited = match self
                .engine
                .transact(self.doc, "Brush stroke", ops.clone(), rev)
            {
                Ok(edited) => edited,
                Err(e) => {
                    // The display may still show the stroke that was not committed.
                    self.renderer.invalidate(None);
                    return Err(error(e));
                }
            };
            self.journal.push(
                self.engine
                    .journal_record("Brush stroke", &ops)
                    .map_err(error)?,
            );
            edited
        } else {
            alto_doc::ChangeSet::default()
        };
        self.refresh()?;
        self.patch_view(&edited)
    }
    #[wasm_bindgen(js_name=cancelBrush)]
    pub fn cancel_brush(&mut self) {
        self.renderer.cancel_stroke();
        self.sampler = None;
        self.layer = None;
    }
    #[wasm_bindgen(js_name=select)]
    pub fn select(&mut self, spec: &str) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: finish tool first"));
        }
        let v = serde_json::from_str(spec).map_err(error)?;
        let ops = self.engine.selection_ops(self.doc, &v).map_err(error)?;
        self.command("Selection", ops)
    }
    #[wasm_bindgen(js_name=selectionOutline)]
    pub fn selection_outline(&self) -> Result<String, JsValue> {
        self.engine.selection_outline(self.doc).map_err(error)
    }
    #[wasm_bindgen(js_name=setMask)]
    pub fn set_mask(&mut self, layer: &str, source: &str) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: finish tool first"));
        }
        let id: NodeId = serde_json::from_str(&format!("\"{layer}\"")).map_err(error)?;
        let linked = self
            .engine
            .document(self.doc)
            .map_err(error)?
            .node(id)
            .map_err(error)?
            .props
            .mask_linked;
        let mask = match source {
            "remove" => None,
            "white" | "selection" => Some(
                self.engine
                    .mask_for_node(self.doc, id, source == "white", linked)
                    .map_err(error)?,
            ),
            _ => return Err(error("INVALID_ARGUMENT: mask source")),
        };
        self.command(
            "Layer mask",
            vec![
                Op::SetMask { layer: id, mask },
                Op::SetProps {
                    id,
                    patch: alto_doc::PropPatch {
                        mask_transform: Some(Affine2::default()),
                        ..Default::default()
                    },
                },
            ],
        )
    }
    #[wasm_bindgen(js_name=setEffects)]
    pub fn set_effects(
        &mut self,
        layer: &str,
        effects: &str,
        use_selection: bool,
    ) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: finish tool first"));
        }
        let id: NodeId = serde_json::from_str(&format!("\"{layer}\"")).map_err(error)?;
        let effects = serde_json::from_str(effects).map_err(error)?;
        let ops = self
            .engine
            .effect_ops(self.doc, id, effects, use_selection)
            .map_err(error)?;
        self.command("Non-destructive effects", ops)
    }
    #[wasm_bindgen(js_name=importRgba)]
    pub fn import_rgba(
        &mut self,
        w: u32,
        h: u32,
        pixels: &[u8],
        encoded: &[u8],
        mime: &str,
    ) -> Result<String, JsValue> {
        if w == 0
            || h == 0
            || w > 32768
            || h > 32768
            || w as u64 * h as u64 > 67_108_864
            || pixels.len() as u64 != w as u64 * h as u64 * 4
        {
            return Err(error("INVALID_ARGUMENT: image dimensions"));
        }
        let mut map = TileMap::new(PixelFormat::Rgba8);
        for ty in 0..h.div_ceil(256) {
            for tx in 0..w.div_ceil(256) {
                let mut tile = vec![0u8; 262144];
                for y in 0..256.min(h - ty * 256) {
                    let count = 256.min(w - tx * 256) as usize * 4;
                    let start = (((ty * 256 + y) * w + tx * 256) * 4) as usize;
                    let dst = y as usize * 1024;
                    tile[dst..dst + count].copy_from_slice(&pixels[start..start + count]);
                }
                if let Some(hash) = self.engine.put_tile(
                    TileData::new(PixelFormat::Rgba8, tile)
                        .map_err(|_| error("INVALID_ARGUMENT: tile"))?,
                ) {
                    self.engine.pin_import(hash);
                    map.tiles.insert(
                        TileKey {
                            tx: tx as i32,
                            ty: ty as i32,
                        },
                        hash,
                    );
                }
            }
        }
        let hash = self.engine.put_import_blob(encoded.to_vec());
        let record = AssetRecord {
            id: AssetId::new(),
            hash,
            kind: AssetKind::EncodedImage { mime: mime.into() },
            byte_len: encoded.len() as u64,
            dims: Some((w, h)),
        };
        Ok(json!({"record":record,"tiles":map,"width":w,"height":h}).to_string())
    }
    #[wasm_bindgen(js_name=savePackage)]
    pub fn save_package(&mut self) -> Result<Vec<u8>, JsValue> {
        if self.sampler.is_some() {
            return Err(error("CONFLICT: active tool must be flushed"));
        }
        self.engine.save_package(self.doc).map_err(error)
    }
    #[wasm_bindgen(js_name=recoveryOpaque)]
    pub fn recovery_opaque(&self) -> Result<String, JsValue> {
        serde_json::to_string(
            &self
                .engine
                .recovery_opaque_hashes(self.doc)
                .map_err(error)?,
        )
        .map_err(error)
    }
    /// Only called by the journal worker after verifying the checkpoint digest.
    #[wasm_bindgen(js_name=openRecoveryPackage)]
    pub fn open_recovery_package(&mut self, bytes: &[u8], opaque: &str) -> Result<String, JsValue> {
        let opaque: std::collections::BTreeSet<ContentHash> =
            serde_json::from_str(opaque).map_err(error)?;
        let mut engine = Engine::new();
        let (doc, warnings) = engine
            .open_recovery_package(bytes, &opaque)
            .map_err(error)?;
        self.install(engine, doc, warnings)
    }
    #[wasm_bindgen(js_name=openPackage)]
    pub fn open_package(&mut self, bytes: &[u8]) -> Result<String, JsValue> {
        let mut engine = Engine::new();
        let (doc, warnings) = engine.open_package(bytes).map_err(error)?;
        self.install(engine, doc, warnings)
    }
    #[wasm_bindgen(js_name=takeJournal)]
    pub fn take_journal(&mut self) -> Vec<u8> {
        if self.journal.is_empty() {
            vec![]
        } else {
            self.journal.remove(0)
        }
    }
    #[wasm_bindgen(js_name=replayRecord)]
    pub fn replay_record(&mut self, bytes: &[u8]) -> Result<String, JsValue> {
        self.engine.replay_record(self.doc, bytes).map_err(error)?;
        self.refresh()?;
        self.view()
    }
    #[wasm_bindgen(js_name=markSaved)]
    pub fn mark_saved(&mut self, hash: &str) -> Result<String, JsValue> {
        self.saved = hash.parse().map_err(error)?;
        self.view()
    }
    #[wasm_bindgen(js_name=destroyGpu)]
    pub fn destroy_gpu(&self) {
        self.renderer.destroy_device();
    }
    #[wasm_bindgen(js_name=recoverGpu)]
    pub async fn recover_gpu(&mut self) -> Result<String, JsValue> {
        self.cancel_brush();
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::OffscreenCanvas(self.canvas.clone()))
            .map_err(error)?;
        let glyph_atlas = self.renderer.glyph_atlas();
        self.renderer = Renderer::new(
            &instance,
            surface,
            self.canvas.width(),
            self.canvas.height(),
            self.budget,
        )
        .await
        .map_err(error)?;
        self.renderer.set_clock(clock);
        // The new device starts with an empty glyph atlas; entries are rasterized
        // again from the scene's outlines on the next frame.
        self.renderer.set_glyph_atlas(glyph_atlas);
        self.view()
    }
    #[wasm_bindgen(js_name=parseVectorPath)]
    pub fn parse_vector_path(&self, svg: &str) -> Result<String, JsValue> {
        serde_json::to_string(&alto_vector::PathData::from_svg(svg).map_err(error)?).map_err(error)
    }
    #[wasm_bindgen(js_name=vectorPathSvg)]
    pub fn vector_path_svg(&self, data: &str) -> Result<String, JsValue> {
        let p: alto_vector::PathData = serde_json::from_str(data).map_err(error)?;
        p.validate().map_err(error)?;
        Ok(p.to_svg())
    }
    #[wasm_bindgen(js_name=vectorStats)]
    pub fn vector_stats(&self) -> String {
        let (hits, misses, entries, bytes) = self.renderer.vector_stats();
        let g = self.renderer.glyph_stats();
        serde_json::json!({"hits":hits,"misses":misses,"entries":entries,"gpuBytes":bytes,
            "glyphAtlas":{"entries":g.entries,"shelves":g.shelves,"rasterized":g.rasterized,
                "hits":g.hits,"evicted":g.evicted,"resets":g.resets,"atlasGlyphs":g.atlas_glyphs,
                "vectorRuns":g.vector_runs,"fallbackGlyphs":g.fallback_glyphs}})
        .to_string()
    }
    /// Small text through the glyph atlas (default) or always through the vector path.
    #[wasm_bindgen(js_name=setGlyphAtlas)]
    pub fn set_glyph_atlas(&mut self, enabled: bool) {
        self.renderer.set_glyph_atlas(enabled);
    }
    #[wasm_bindgen(js_name=readTileLevel)]
    pub async fn read_tile_level(
        &mut self,
        level: i32,
        tx: i32,
        ty: i32,
    ) -> Result<Vec<u8>, JsValue> {
        if !(-6..=12).contains(&level)
            || tx.unsigned_abs() > 1_000_000
            || ty.unsigned_abs() > 1_000_000
        {
            return Err(error("INVALID_ARGUMENT: display tile"));
        }
        self.renderer
            .read_tile(&self.scene, &self.engine, (level, tx, ty))
            .await
            .map_err(error)
    }
    #[wasm_bindgen(js_name=readTile)]
    pub async fn read_tile(&mut self, tx: i32, ty: i32) -> Result<Vec<u8>, JsValue> {
        self.renderer
            .read_tile(&self.scene, &self.engine, (0, tx, ty))
            .await
            .map_err(error)
    }
}
impl Runtime {
    pub(crate) fn install(
        &mut self,
        engine: Engine,
        doc: DocHandle,
        warnings: Vec<String>,
    ) -> Result<String, JsValue> {
        self.renderer.cancel_stroke();
        self.engine = engine;
        self.doc = doc;
        self.sampler = None;
        self.layer = None;
        self.journal.clear();
        self.warnings = warnings;
        self.saved = self.state_hash()?;
        let meta = &self.engine.document(doc).map_err(error)?.meta;
        self.viewport.center = [meta.width as f64 / 2.0, meta.height as f64 / 2.0];
        self.reload()?;
        self.view()
    }
    /// Identity of what a save writes: the snapshot and the version head.
    pub(crate) fn state_hash(&self) -> Result<ContentHash, JsValue> {
        let mut b = ContentHash::of(&self.engine.snapshot(self.doc).map_err(error)?)
            .0
            .to_vec();
        if let Some(h) = self.engine.version_status(self.doc).map_err(error)?.head {
            b.extend(h.0);
        }
        Ok(ContentHash::of(&b))
    }
    fn command(&mut self, intent: &str, ops: Vec<Op>) -> Result<String, JsValue> {
        let revision = self.engine.document(self.doc).map_err(error)?.revision;
        let edited = self
            .engine
            .transact(self.doc, intent, ops.clone(), revision)
            .map_err(error)?;
        self.journal
            .push(self.engine.journal_record(intent, &ops).map_err(error)?);
        self.refresh()?;
        self.patch_view(&edited)
    }
    /// After an edit of the same document: discard only the display tiles it can change.
    fn refresh(&mut self) -> Result<(), JsValue> {
        let scene = self.engine.render_scene(self.doc).map_err(error)?;
        let damage = alto_scene::damage::scene_damage(&self.scene, &scene);
        self.scene = scene;
        self.renderer.invalidate_damage(&damage);
        Ok(())
    }
    /// After another document replaces the current one.
    fn reload(&mut self) -> Result<(), JsValue> {
        self.scene = self.engine.render_scene(self.doc).map_err(error)?;
        self.renderer.invalidate(None);
        Ok(())
    }
}
#[wasm_bindgen]
impl Runtime {
    /// Exercises an uncached frame under a WebGPU validation error scope.
    #[wasm_bindgen(js_name=validateFrame)]
    pub async fn validate_frame(&mut self) -> Result<(), JsValue> {
        let scope = self
            .renderer
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        self.renderer.invalidate(None);
        loop {
            let frame = self
                .renderer
                .render(&self.scene, &self.engine, self.viewport, 1000.0)
                .map_err(error)?;
            if frame.pending == 0 {
                break;
            }
        }
        self.gpu_idle().await?;
        if let Some(validation) = scope.pop().await {
            return Err(error(format!("INTERNAL: GPU validation: {validation}")));
        }
        Ok(())
    }
    #[wasm_bindgen(js_name=gpuIdle)]
    pub async fn gpu_idle(&self) -> Result<(), JsValue> {
        let (tx, rx) = futures_channel::oneshot::channel();
        self.renderer.queue.on_submitted_work_done(move || {
            let _ = tx.send(());
        });
        rx.await.map_err(error)
    }
}
#[wasm_bindgen]
impl Runtime {
    #[wasm_bindgen(js_name=cpuBrushReference)]
    pub fn cpu_brush_reference(
        &self,
        config: &str,
        color: &[f32],
        samples: &[f32],
    ) -> Result<Vec<u8>, JsValue> {
        let cfg: BrushConfig = serde_json::from_str(config).map_err(error)?;
        if color.len() != 4 || !samples.len().is_multiple_of(6) {
            return Err(error("INVALID_ARGUMENT: reference input"));
        }
        let mut s = StrokeSampler::new(cfg);
        let mut dabs = vec![];
        for p in samples.as_chunks::<6>().0 {
            dabs.extend(s.input(StrokeSample {
                pos: [p[0], p[1]],
                pressure: p[2],
                tilt: [p[3], p[4]],
                t_ms: p[5] as f64,
            }));
        }
        let mut out = alto_raster::cpu_paint(
            &vec![0; 262144],
            [0.0, 0.0],
            &dabs,
            cfg,
            [color[0], color[1], color[2], color[3]],
            false,
        );
        for p in out.as_chunks_mut::<4>().0 {
            for i in 0..3 {
                p[i] = ((p[i] as f32 * p[3] as f32) / 255.0).round() as u8;
            }
        }
        Ok(out)
    }
}

mod m78;
#[cfg(feature = "vello-probe")]
mod vector_probe;
