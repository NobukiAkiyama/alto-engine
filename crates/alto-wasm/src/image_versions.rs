//! Protocol 6 image and history commands.
use crate::{Runtime, error};
use alto_core::{ContentHash, NodeId};
use alto_engine::image::ImageEdit;
use serde::Deserialize;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;
fn hash(s: &str) -> Result<ContentHash, JsValue> {
    s.parse()
        .map_err(|_| error("INVALID_ARGUMENT: commit hash"))
}
fn id(s: &str) -> Result<NodeId, JsValue> {
    serde_json::from_value(json!(s)).map_err(error)
}
fn parse_edit<'a, T: Deserialize<'a>>(request: &'a str) -> Result<T, JsValue> {
    serde_json::from_str(request).map_err(|e| {
        let code = if e.to_string().contains("unknown variant") {
            "UNSUPPORTED"
        } else {
            "INVALID_ARGUMENT"
        };
        error(format!("{code}: {e}"))
    })
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum HistoryCommand {
    CreateBranch {
        name: String,
        from: Option<String>,
        #[serde(default)]
        checkout: bool,
    },
    Checkout {
        name: String,
    },
    Detach {
        hash: String,
    },
    DeleteBranch {
        name: String,
        #[serde(default)]
        force: bool,
    },
    ProtectBranch {
        name: String,
        protected: bool,
    },
    CreateTag {
        name: String,
        hash: String,
    },
    DeleteTag {
        name: String,
    },
    Pin {
        hash: String,
        pin: bool,
    },
    AdoptGc {
        manifest: String,
    },
    FinishMerge {
        token: String,
        resolutions: Vec<alto_engine::versions::MergeResolution>,
        message: String,
        created_at: i64,
        author: Option<String>,
    },
}
impl Runtime {
    fn apply_prepared<R: serde::Serialize>(
        &mut self,
        intent: &str,
        p: alto_engine::canvas::PreparedOps<R>,
    ) -> Result<String, JsValue> {
        let result = serde_json::to_value(&p.result).map_err(error)?;
        let state = if p.ops.is_empty() {
            self.view()
        } else {
            self.command(intent, p.ops)
        };
        self.engine.release_staged_tiles(p.stage);
        Ok(
            json!({"state":serde_json::from_str::<Value>(&state?).map_err(error)?,"result":result})
                .to_string(),
        )
    }
}
#[wasm_bindgen]
impl Runtime {
    #[wasm_bindgen(js_name=imageEdit)]
    pub fn image_edit(&mut self, request: &str) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("BUSY: finish active tool first"));
        }
        let edit: ImageEdit = parse_edit(request)?;
        let intent = edit.intent();
        let p = self
            .engine
            .prepare_image_edit(self.doc, edit)
            .map_err(error)?;
        let result = serde_json::to_value(&p.result).map_err(error)?;
        let state = if p.ops.is_empty() {
            self.view()
        } else {
            self.command(intent, p.ops)
        };
        self.engine.release_staged_tiles(p.stage);
        Ok(
            json!({"state":serde_json::from_str::<Value>(&state?).map_err(error)?,"result":result})
                .to_string(),
        )
    }
    #[wasm_bindgen(js_name=canvasEdit)]
    pub fn canvas_edit(&mut self, request: &str) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("BUSY: finish active tool first"));
        }
        let edit: alto_engine::canvas::CanvasEdit = parse_edit(request)?;
        let intent = edit.intent();
        let p = self
            .engine
            .prepare_canvas_edit(self.doc, edit)
            .map_err(error)?;
        self.apply_prepared(intent, p)
    }
    #[wasm_bindgen(js_name=layerEdit)]
    pub fn layer_edit(&mut self, request: &str) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("BUSY: finish active tool first"));
        }
        let edit: alto_engine::canvas::LayerEdit = parse_edit(request)?;
        let intent = edit.intent();
        let p = self
            .engine
            .prepare_layer_edit(self.doc, edit, &|paths, world, window| {
                alto_gpu::raster_q8::vector_pixels_q14(paths, world, window)
                    .map_err(|_| "the vector is outside the renderer's coordinate range".into())
            })
            .map_err(error)?;
        self.apply_prepared(intent, p)
    }
    #[wasm_bindgen(js_name=imageHistogram)]
    pub fn image_histogram(&self, node: &str, range: &str) -> Result<String, JsValue> {
        let range = if range.is_empty() {
            None
        } else {
            Some(parse_edit(range)?)
        };
        serde_json::to_string(
            &self
                .engine
                .image_histogram(self.doc, id(node)?, range)
                .map_err(error)?,
        )
        .map_err(error)
    }
    #[wasm_bindgen(js_name=gcPlan)]
    pub fn gc_plan(&self, framed: &[u8], dry_run: bool) -> Result<Vec<u8>, JsValue> {
        if self.sampler.is_some() {
            return Err(error("BUSY: finish active tool first"));
        }
        let plan = self
            .engine
            .prepare_gc(self.doc, &crate::versions::unframe(framed)?, dry_run)
            .map_err(error)?;
        let mut entries = plan.entries;
        entries.insert(
            ".gc-plan.json".into(),
            serde_json::to_vec(&json!({"report":plan.report,"remove":plan.remove}))
                .map_err(error)?,
        );
        Ok(crate::versions::frame(&entries))
    }
    #[wasm_bindgen(js_name=compactArchive)]
    pub fn compact_archive(&self, bytes: &[u8]) -> Result<Vec<u8>, JsValue> {
        if self.sampler.is_some() {
            return Err(error("BUSY: finish active tool first"));
        }
        let (bytes, report) = self
            .engine
            .compact_archive(self.doc, bytes)
            .map_err(error)?;
        Ok(crate::versions::frame(&std::collections::BTreeMap::from([
            ("archive.alto".into(), bytes),
            (
                "report.json".into(),
                serde_json::to_vec(&report).map_err(error)?,
            ),
        ])))
    }
    #[wasm_bindgen(js_name=importSvg)]
    pub fn import_svg(&mut self, svg: &str, parent: &str, index: u32) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("BUSY: finish active tool first"));
        }
        let p = self
            .engine
            .prepare_svg_import(self.doc, svg, id(parent)?, index as usize)
            .map_err(error)?;
        let result = serde_json::to_value(p.result).map_err(error)?;
        let state: Value =
            serde_json::from_str(&self.command("image.import_svg", p.ops)?).map_err(error)?;
        Ok(json!({"result":result,"state":state}).to_string())
    }
    #[wasm_bindgen(js_name=pickColor)]
    pub fn pick_color(&self, node: &str, x: f64, y: f64) -> Result<String, JsValue> {
        serde_json::to_string(
            &self
                .engine
                .pick_color(self.doc, id(node)?, [x, y])
                .map_err(error)?,
        )
        .map_err(error)
    }
    #[wasm_bindgen(js_name=pickComposite)]
    pub async fn pick_composite(&mut self, x: i32, y: i32) -> Result<String, JsValue> {
        let doc = self.engine.document(self.doc).map_err(error)?;
        if x < 0 || y < 0 || x as u32 >= doc.meta.width || y as u32 >= doc.meta.height {
            return Err(error("NOT_FOUND: pixel outside canvas"));
        }
        let tile = self
            .renderer
            .read_tile(
                &self.scene,
                &self.engine,
                (0, x.div_euclid(256), y.div_euclid(256)),
            )
            .await
            .map_err(error)?;
        let i = (y.rem_euclid(256) as usize * 256 + x.rem_euclid(256) as usize) * 4;
        let p = &tile[i..i + 4];
        let a = u32::from(p[3]);
        let mut bytes = [0u8; 4];
        bytes[3] = p[3];
        for c in 0..3 {
            bytes[c] = (u32::from(p[c]) * 255 + a / 2)
                .checked_div(a)
                .unwrap_or(0)
                .min(255) as u8;
        }
        Ok(json!({"bytes":bytes,"color":bytes.map(|b|b as f64/255.0)}).to_string())
    }
    #[wasm_bindgen(js_name=listBranches)]
    pub fn list_branches(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.engine.list_branches(self.doc).map_err(error)?).map_err(error)
    }
    #[wasm_bindgen(js_name=prepareMerge)]
    pub fn prepare_merge(&mut self, name: &str) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("BUSY: finish active tool first"));
        }
        serde_json::to_string(&self.engine.prepare_merge(self.doc, name).map_err(error)?)
            .map_err(error)
    }
    #[wasm_bindgen(js_name=cancelMerge)]
    pub fn cancel_merge(&mut self) -> Result<(), JsValue> {
        self.engine.cancel_merge(self.doc).map_err(error)
    }
    #[wasm_bindgen(js_name=historyCommand)]
    pub fn history_command(&mut self, request: &str) -> Result<String, JsValue> {
        if self.sampler.is_some() {
            return Err(error("BUSY: finish active tool first"));
        }
        let command: HistoryCommand =
            serde_json::from_str(request).map_err(|e| error(format!("INVALID_ARGUMENT: {e}")))?;
        let is_gc = matches!(&command, HistoryCommand::AdoptGc { .. });
        let previous_saved = self.saved;
        let checkpoint = self.engine.capture_version_state(self.doc).map_err(error)?;
        let outcome = (|| -> Result<Value, JsValue> {
            let result = match command {
                HistoryCommand::CreateBranch {
                    name,
                    from,
                    checkout,
                } => {
                    self.engine
                        .create_branch(
                            self.doc,
                            &name,
                            from.as_deref().map(hash).transpose()?,
                            checkout,
                        )
                        .map_err(error)?;
                    Value::Null
                }
                HistoryCommand::Checkout { name } => {
                    self.engine.checkout(self.doc, &name).map_err(error)?;
                    Value::Null
                }
                HistoryCommand::Detach { hash: s } => {
                    self.engine.detach(self.doc, hash(&s)?).map_err(error)?;
                    Value::Null
                }
                HistoryCommand::DeleteBranch { name, force } => {
                    self.engine
                        .delete_branch(self.doc, &name, force)
                        .map_err(error)?;
                    Value::Null
                }
                HistoryCommand::ProtectBranch { name, protected } => {
                    self.engine
                        .protect_branch(self.doc, &name, protected)
                        .map_err(error)?;
                    Value::Null
                }
                HistoryCommand::CreateTag { name, hash: s } => {
                    self.engine
                        .set_tag(self.doc, &name, Some(hash(&s)?))
                        .map_err(error)?;
                    Value::Null
                }
                HistoryCommand::DeleteTag { name } => {
                    self.engine.set_tag(self.doc, &name, None).map_err(error)?;
                    Value::Null
                }
                HistoryCommand::Pin { hash: s, pin } => {
                    self.engine
                        .pin_version(self.doc, hash(&s)?, pin)
                        .map_err(error)?;
                    Value::Null
                }
                HistoryCommand::AdoptGc { manifest } => {
                    self.engine
                        .adopt_gc(self.doc, manifest.as_bytes())
                        .map_err(error)?;
                    Value::Null
                }
                HistoryCommand::FinishMerge {
                    token,
                    resolutions,
                    message,
                    created_at,
                    author,
                } => serde_json::to_value(
                    self.engine
                        .finish_merge(self.doc, &token, resolutions, &message, created_at, author)
                        .map_err(error)?,
                )
                .map_err(error)?,
            };
            let record = self
                .engine
                .version_record(self.doc, checkpoint.digest)
                .map_err(error)?;
            if is_gc {
                self.saved = self.state_hash()?;
            }
            self.refresh()?;
            let state: Value = serde_json::from_str(&self.view()?).map_err(error)?;
            self.journal.push(record);
            Ok(json!({"state":state,"result":result}))
        })();
        match outcome {
            Ok(v) => {
                self.engine.release_version_state(checkpoint);
                Ok(v.to_string())
            }
            Err(e) => {
                self.engine.rollback_version_state(self.doc, checkpoint);
                self.saved = previous_saved;
                self.refresh()?;
                Err(e)
            }
        }
    }
}
