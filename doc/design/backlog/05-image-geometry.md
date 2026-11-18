# 焼き込みと幾何

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 拒否している焼き込みの条件を減らし、表示用のミップと高度な変形を設計する。

**Architecture:** 確定編集は `crates/alto-engine/src/canvas.rs`（レイヤー単位の焼き込み）と `crates/alto-engine/src/image/`（ラスターの画素編集）の CPU 処理で、`Engine::prepare_layer_edit` と `Engine::prepare_image_edit` が `Op::ReplaceNode` を返す。効果付きの焼き込みは CPU に効果の実装がない（`crates/alto-gpu/src/cpu_composite/` は `#[cfg(test)]` で本番に含まれない）ため、GPU の 1:1 合成を読み戻して正本にする。

## 共通の現状

- 入口：SDK `doc.image.rasterize` / `mergeDown`（`packages/alto-sdk/src/document.ts`）→ プロトコル `layerEdit`（`packages/alto-sdk/src/protocol.ts`）→ `packages/alto-sdk/src/worker.ts` の `execute` → `crates/alto-wasm/src/image_versions.rs` の `layer_edit`（`layerEdit`）→ `Engine::prepare_layer_edit`（`crates/alto-engine/src/canvas.rs`）→ `apply_prepared`（`Runtime::command` で可逆 Op の 1 トランザクション）。画素編集は `imageEdit` → `image_edit` → `Engine::prepare_image_edit`（`crates/alto-engine/src/image/mod.rs`）。
- Worker のキューは直列である。`drain` が 1 要求ずつ `await execute(m)` するため、非同期の要求（`pickComposite`、`exportImage`）の実行中に別の編集は走らない。ただし `Engine::transact` は `base_revision` を検査して食い違いを `CONFLICT` にするので、準備と確定が分かれる経路では必ずこれを渡す。
- 上限（堅牢化 A〜F の後も同じ。`doc/MILESTONES.md`「上限・継続課題」3 と一致）：画像は `MAX_IMAGE_PIXELS`＝64M 画素（`crates/alto-doc/src/image_bounds.rs`）、辺長 65536。画素編集の作業領域は `EDIT_BUDGET`＝256 MiB（`crates/alto-engine/src/image/mod.rs`）で、超えると `RESOURCE_LIMIT`。`readTiles` は 1 回 64 タイルまで、読み戻しの合計は 512 MiB まで（`crates/alto-gpu/src/renderer/tile_read.rs`）。
- エラー：`image::err` の `UNSUPPORTED`／`INVALID_ARGUMENT`／`RESOURCE_LIMIT`／`CONFLICT`。GPU の予算超過は `RenderError::Budget` で `INVALID_ARGUMENT`、デバイス喪失は `GPU_LOST`、v3 に定義の無い機能は `GPU_UNSUPPORTED`（`crates/alto-gpu/src/error.rs`）。
- 表示タイルのキャッシュ（`Renderer::cache_map`）は `(level, tx, ty)` をキーとし、シーンを区別しない。別のシーンを読み戻すと、同じキーの表示タイルを上書きする。IMG-1 と IMG-5 はこの点を避ける。

## IMG-1 属性付きの焼き込み

### 現状

- `plain_props`（`crates/alto-engine/src/canvas.rs`）が、マスク・ベクターマスク・クリップパス（`bake does not apply masks or clipping paths`）、効果（`bake does not apply effects`）、下にクリップ（`bake does not apply clipping`）、非表示（`a hidden layer would be lost`）、Normal 以外の合成モード（`bake supports the Normal blend mode`）を、すべて `UNSUPPORTED` で拒否する。`plain_raster` は `NodeKind::Raster` 以外を `only raster and vector layers can be baked` で拒否する。
- `prepare_rasterize` のラスター枝は、マスクと効果だけを別に拒否し、整数平行移動（`whole_translation`）を画素へ焼く。`prepare_merge_down` は両方を `plain_raster` に通し、上の不透明度を画素へ畳み込む。下の不透明度は 1 に戻して下のノードを残す。上の次の兄弟が `clip_to_below` なら `a layer above is clipped to this layer` で拒否する。
- 読み戻しの部品：`Renderer::read_tiles`（`crates/alto-gpu/src/renderer/tile_read.rs`）は表示タイル（premultiplied RGBA8、256×256）を `(level, tx, ty)` で読む。`crates/alto-wasm/src/lib.rs` の `readTiles` が 64 タイルまでを非同期で返し、`pickComposite`（`crates/alto-wasm/src/image_versions.rs`）が 1 画素の straight 変換を持つ。書き出し（`exportImage`、`packages/alto-sdk/src/worker.ts`）も同じ表示タイルの読み戻しである。
- GPU の効果の段は `crates/alto-gpu/src/renderer/tile_renderer/stages.rs`（`stage`、`input_roi_v3`）にある。旧 `m5.rs` は無い。
- ノードの合成シーンは `Engine::render_scene`（`crates/alto-engine/src/lib.rs`）が文書全体から作る。1 ノードだけのシーンを作る関数は無い（新規）。
- 試験：Rust の `merge_down_composites_and_refuses_what_it_cannot_keep`（`crates/alto-engine/src/canvas.rs`）、ブラウザの `clone stamp / smudge / histogram / merge down / rasterize through the SDK`（`tests/e2e/image-version-tests.js`）。

### 11-04 からの変化

- 旧設計書の GPU 段の置き場（旧 m5.rs）は、`stages.rs`、`compose.rs`、`tile_read.rs` などへ分かれた（4121d69、725d6f4）。
- 旧設計書は「読み戻しの間に別の編集が走ると `CONFLICT`」を試験にしていたが、Worker のキューは直列なので SDK からは起こせない。確定時の `base_revision` 検査だけが残り、試験は Rust に移る。
- `plain_props`、`prepare_rasterize`、`prepare_merge_down` の中身は変化なし。

### 設計

焼き込みを 3 段にする。

1. **計画（同期）。** `Engine::bake_plan(h, node)`（新規）が、検査と範囲の計算をして `BakePlan { revision, region, node }` を返す。
2. **合成（非同期、GPU）。** 対象ノードだけの一時シーンを文書画素 1:1（レベル 0）で描き、`region` のタイルを読み戻す。効果、マスク、クリップパスが効いた結果が得られる。
3. **確定（同期）。** `Engine::bake_finish(h, plan, premultiplied)`（新規）が、読み戻した画素を straight RGBA8 に直し（`(c * 255 + a / 2) / a` を 255 で頭打ち。`pickComposite` と同じ式）、`Op::ReplaceNode` でノードをラスターにする。`plan.revision` を `transact` の `base_revision` に渡す。

**検査（計画）。** 次を満たさないものは `UNSUPPORTED`（理由を文面に入れる）。

- 対象は `NodeKind::Raster`、`NodeKind::Vector`、`NodeKind::Text`。グループは対象にしない（グループは従来の `rasterize` の平坦化のまま）。
- 下にクリップ（`clip_to_below`）は拒否を維持する。基底のアルファに依存するので、基底の範囲を焼く選択肢は作らない。
- 非表示は `a hidden layer would be lost` で拒否を維持する。
- 祖先の変形が整数平行移動だけであること（`pixel_aligned_world` と同じ検査）。ノード自身の変形は任意でよい（IMG-2 と同じ扱い。下の「ノードの変形」）。
- 次の兄弟が `clip_to_below` ならその旨で拒否する（`prepare_merge_down` と同じ）。
- 祖先のロックは `INVALID_ARGUMENT`（`unlocked`）。

**一時シーン。** 根（恒等、Isolated）の下にノード 1 つ。ノードは不透明度 1、合成モード Normal、`clip_to_below` なし、変形は「祖先の平行移動 ∘ 自身の変形」、効果・マスク・クリップパス・ベクターマスクはそのまま。祖先グループの不透明度・合成モード・マスク・効果は焼かない（文書に明記する）。`Engine::render_scene` の内部関数 `append` を、任意の 1 ノードを根の子にして作れる形に分ける（`Engine::bake_scene`（新規））。一時シーンは `region` の左上が (0, 0) になるよう `-region.origin` だけ平行移動して作り、`scene.canvas` は `{ x0: 0, y0: 0, x1: region の幅, y1: region の高さ }` にする。描画は `canvas.x1`／`canvas.y1` だけを上限に使い、`canvas.x0`／`canvas.y0` は ROI の切り詰めにしか効かないので、負の原点の `region`（文書の左・上へはみ出す効果）は平行移動しないと透明で返る。確定で原点を足し戻す。

**範囲。** `region` ＝ ノードの文書座標の外接矩形 ∪ 効果の出力範囲。効果の出力範囲は FX-5 の `effect_output_bounds(effects: &[REffect], bounds: IRect) -> Option<IRect>`（新規。02 の FX-5 が作る）を使う。`None`（桁あふれ）は `RESOURCE_LIMIT`。FX-5 が入るまでは、ぼかしの `radius`（0〜64 文書 px）で広げた矩形を使う。上限は `MAX_IMAGE_PIXELS` と、タイル数 `MAX_BAKE_TILES = 256`（新規、64 MiB。読み戻しのバッファ、straight の出力タイル、`stage_tile` の分で `EDIT_BUDGET` に収める。マージダウンは 2 バッファを数えるので、`region` のタイル数 × 2 も 256 以下にする）。超えたら `RESOURCE_LIMIT`。範囲は文書の外へ広がってよい。（新規）

**読み戻し。** `Renderer::read_scene_tiles_uncached(scene, store, keys)`（新規。`crates/alto-gpu/src/renderer/tile_read.rs`）が、`prepare_scene` のあとタイルごとに `compose_tiles` で描き、表示キャッシュの `cache_map` へは登録しない。既定：同じキーの既存エントリを先に `cache_map` から外してスロットを解放し、読んだ後にも外す（実画面のタイルは次のフレームで描き直す）。代案：予約した専用スロットを 1 つ持つ。`cache_slot` の割り当てを読んで、どちらが小さい変更かを実装者が選ぶ。1 回の呼び出しで読むタイルは `readbackCapacity()`（`crates/alto-wasm/src/lib.rs`）以下。GPU の失敗（`GPU_LOST`、`INVALID_ARGUMENT` の予算超過）はそのまま返す。この時点で文書は変わっていない。

**確定で残す属性。** 新しいラスターは、ノード ID・名前・不透明度・合成モード・ロック・親子関係を保つ。変形は恒等（祖先の平行移動を除く局所座標で、原点が文書画素に揃う）、効果・マスク・ベクターマスク・クリップパスは空（`mask_enabled`、`mask_linked`、`mask_transform` は既定へ戻す）。画像範囲 `ImageBounds` は、読み戻した結果の非透明な外接矩形に縮める（全透明ならタイル無しで範囲は `region` のままにする）。

**8bit の量子化。** 表示タイルは Q14 の合成結果を 8bit の premultiplied に丸めたものである。焼いた後の見た目は、Q14 の中間結果と比べて 1/255 までずれうる。効果の連鎖の途中では一致しない。試験の条件は、焼く前の読み戻し（premultiplied）と、焼いた後の読み戻しの各チャンネルの差が 1 以内、アルファが 255 の画素は全バイト一致、とする。straight に直して premultiplied へ戻す丸めが効くのは、アルファが小さい画素に限られる。

**マージダウン。** 同じ機構を使い、`mergeDown` の拒否条件を減らす。

- 従来の CPU 経路（両方 Normal・効果なし・マスクなし）は変えない。同期のまま、画素も変えない。
- 新しい条件：上が Normal 以外の合成モード、またはどちらかに効果・マスクがある場合は、GPU 経路にする。一時シーンは根の下に「下、上」の順（各々の属性つき、ただし下の不透明度 1・Normal に読み替える）を置く。次を満たすときだけ許す。
  - 下が Normal で、不透明度 1（効果・マスクは焼かれた結果で見る）、`clip_to_below` なし（`plain_props` と同じく `bake does not apply clipping` で拒否。下の基底が消えるため）。
  - 上が Normal 以外のとき、上の出力範囲のすべての画素で、下だけを描いた読み戻しのアルファが 255。この判定のため、マージの `bake_read` は「下だけ」と「下＋上」の 2 シーンを順に読み、2 つのバッファを返す。`region` は下と上の `region` の和で、`bake_plan` が計算する（判定そのものは読み戻しの後の `bake_finish` が行い、満たさなければ `UNSUPPORTED`）。背後の合成と畳み込み後の合成が一致するのはこのときだけだからである。透明な範囲に Normal 以外の上が掛かると、画面上は下の層との合成が変わる。（新規）
  - 満たさないときは `UNSUPPORTED`（`merge down would look different`）で、文書は変わらない。
- 結果は下のノードを残し、上を `Op::RemoveNode` で消す（従来と同じ）。下のノードは `bake_finish` と同じく、効果・マスク・ベクターマスク・クリップパスを空にし、不透明度 1・Normal にして置き換える。一時シーンは永続の属性から作られるので、空にしないと焼いた結果へ二重に掛かる。（新規）

**ノードの変形（IMG-2 との関係）。** 焼く対象の自身の変形が整数平行移動でない場合、一時シーンはその変形で描き、確定で恒等にする。このときの属性の読み替え（リンクされたマスクの変形など）は IMG-2 の「属性の移し替え」と同じ関数を使う（`bake_relocate`、新規）。（新規）

**SDK。**

```ts
doc.image.rasterize(node, options?: { bakeProps?: boolean }): Promise<LayerEditResult>
```

- `bakeProps` が偽（既定）なら従来の `layerEdit`（同期の経路）で、結果も変わらない。真なら新要求 `layerBake` を呼ぶ。（新規）
- `mergeDown(node)` は、従来の条件に合えば `layerEdit`、合わなければ自動で `layerBake` に回る。回してよい条件は上記の GPU 経路の条件。（新規）
- `LayerEditResult` は `{ node, bounds, no_op }`。IMG-3 が `missing` を足す。

**Worker と wasm。**

```ts
// packages/alto-sdk/src/protocol.ts
layerBake: Q<{ node: NodeId; merge?: boolean }, { state: WireState; result: LayerEditResult }>;
```

```rust
// crates/alto-wasm/src/image_versions.rs（いずれも新規）
fn bake_plan(&mut self, node: &str, merge: bool) -> Result<String, JsValue>;           // 同期。BUSY 検査つき
async fn bake_read(&mut self, node: &str, merge: bool) -> Result<Vec<u8>, JsValue>;    // 非同期。タイルを順に連結
fn bake_finish(&mut self, node: &str, merge: bool, revision: u64, bytes: &[u8]) -> Result<String, JsValue>;
```

Worker の `execute` は 3 つを続けて呼び、最後の結果に `queueJournal(); postMessage({ t: "patch", value: r.state })` を付ける（`layerEdit` と同じ）。`bake_read` の途中で失敗しても文書は変わらない。`self.sampler.is_some()` のときは `BUSY`（他の編集系と同じ）。（新規）

**エラー。** 条件に合わない：`UNSUPPORTED`。範囲が大きい：`RESOURCE_LIMIT`。ロック：`INVALID_ARGUMENT`。読み戻し中に版が進んだ：`CONFLICT`。GPU 喪失：`GPU_LOST`（Worker の `recoverLostGpu` が再構築し、要求はエラーで返る。文書は変わらない）。

### 形式と通信

- `format_version`：上げない。結果は通常のラスターで、新しいフィールドは無い。
- `PROTOCOL`：上げる（新要求 `layerBake`）。値は 11（段階2の回。00-ROADMAP の番号の表）。`crates/alto-wasm/src/lib.rs` の `protocol_version`（`protocolVersion`）と `packages/alto-sdk/src/protocol.ts` の両方に反映する。（新規）
- 描画：v3 の画素は変えない。GPU の新しい段は作らない。`tests/parity/` に行は足さない。

### 作業手順

**Files:**
- Modify：`crates/alto-gpu/src/renderer/tile_read.rs`（`Renderer::read_scene_tiles_uncached`（新規））、`crates/alto-engine/src/canvas.rs`（`BakePlan`、`Engine::bake_plan`、`Engine::bake_finish`、`MAX_BAKE_TILES`、マージダウンの条件）、`crates/alto-engine/src/lib.rs`（`Engine::bake_scene`、`append` の分離）、`crates/alto-wasm/src/image_versions.rs`（`bake_plan`、`bake_read`、`bake_finish`）、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/image-types.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`tests/e2e/image-version-tests.js`。

**Interfaces:** `Engine::bake_plan(h: DocHandle, id: NodeId, merge: bool) -> Result<BakePlan, EngineError>`、`Engine::bake_finish(h, plan: BakePlan, premultiplied: &[u8]) -> Result<PreparedOps<LayerEditResult>, EngineError>`。Consumes `effect_output_bounds`（02 の FX-5）。

- [ ] 試験 `bake_plan_refuses_clip_hidden_and_non_translation_ancestors`（Rust）：下にクリップ、非表示、祖先が回転、ロック済み、次の兄弟がクリップ、の各場合に `UNSUPPORTED`／`INVALID_ARGUMENT` で、`snapshot` が変わらない。
- [ ] 試験 `bake_plan_region_includes_effect_bounds_and_is_limited`（Rust）：ぼかし半径 64 の範囲が `region` に入り、`MAX_BAKE_TILES` 超過が `RESOURCE_LIMIT`。
- [ ] 試験 `bake_finish_clears_props_and_keeps_opacity_blend_and_id`（Rust）：効果・マスク・クリップパスが空、不透明度・合成モード・ID・名前が残り、1 回の Undo で戻る。
- [ ] 試験 `bake_finish_conflicts_when_the_revision_moved`（Rust）：計画の後で別の編集を `transact` してから確定すると `CONFLICT`、文書は変わらない。
- [ ] 試験 `merge_down_refuses_when_it_would_look_different`（Rust）：下が透明を含み上が Multiply なら `UNSUPPORTED`。下が全面不透明なら通る。
- [ ] ブラウザ試験 `baking a blurred masked layer looks the same`：焼く前の `pickComposite` の列と、焼いた後の列が各チャンネル 1 以内で一致（アルファ 255 の画素は全バイト一致）。ぼかしの外へ広がった範囲も含める。
- [ ] ブラウザ試験 `baking refuses a clipped layer`：`UNSUPPORTED`、`snapshotHash` 不変。
- [ ] ブラウザ試験 `GPU loss during a bake leaves the document unchanged`：`injectReadbackFailure` はストローク終了の読み戻しにしか配線されていない（`crates/alto-gpu/src/renderer/readback.rs` の `fail_after`）。同じ `readback_fault` を `read_scene_tiles_uncached`（新規）も消費するよう配線し、これで読み戻しを失敗させ、`snapshotHash` 不変。
- [ ] 試験 `baking does not disturb the visible tiles`：文書の範囲内の同じタイル `(0, tx, ty)` を、焼く前に表示で描いておき、効果つきノードを焼いた後に同じタイルを読み戻す。焼いた後のタイルが、新しい `Renderer`（キャッシュ空）で同じ文書を描いた同じタイルと全バイト一致する。焼く前と比べない（文書が変わり、量子化の差も出るため）。衝突は同じキーでしか起きないので、同じ `(0, tx, ty)` で比べる。
- [ ] 実装、PR。

**完了条件：** 上の試験が通る。既存の `rasterize` と `mergeDown` の試験が無修正で通る。`tests/parity/cpu-reference.txt`、`tests/parity/v3-browser.txt` が不変。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：02 の FX-5（`effect_output_bounds`）。無い間は上記の暫定範囲で進められる。（新規）
- 後に続く項目：IMG-5（非同期の「準備・読み戻し・確定」の機構を再利用する）。IMG-2、IMG-3 の `bake_relocate` と共有する。（新規）

## IMG-2 任意変形のベクターのラスタライズ

### 現状

- `prepare_rasterize` のベクター枝（`crates/alto-engine/src/canvas.rs`）は、`pixel_aligned_world` で「祖先とノードの変形の合成が整数平行移動」であることを求め、そうでなければ `a vector can be rasterized only at a whole-pixel position at scale 1` で `UNSUPPORTED` にする。結果はノードの種類だけを `Raster` に替え、変形・不透明度・合成モード・マスク・効果をノードに残す。
- `vector_extent` がパスのノード局所の範囲（線幅と miter の余白つき）を返し、`vector_tiles` がタイルごとに `VectorRasterizer` を呼ぶ。ウィンドウは「タイル位置 ＋ 整数オフセット」の文書画素である。
- `VectorRasterizer`（`crates/alto-engine/src/canvas.rs`）は `&dyn Fn(&[StyledPath], FixedAffine, [i64; 4]) -> Result<Vec<[u16; 4]>, String>`。wasm 側が `alto_gpu::raster_q8::vector_pixels_q14` を渡す（`crates/alto-wasm/src/image_versions.rs` の `layer_edit`）。この関数は任意の `FixedAffine` を受け取る。
- 試験：Rust の `rasterize_bakes_offsets_and_flattens_raster_groups`、ブラウザの `Catmull-Rom and area filters / vector layers and groups become pixels with the same picture`（ここで拡大 2 倍の変形が `UNSUPPORTED` になることを確かめている）。

### 11-04 からの変化

- `crates/alto-gpu/src/raster_q8.rs` のラスタライザは堅牢化 E で内部が変わった（`rasterize_crop`、`Prepared`、非ゼロの積分を i64 から始めて溢れたら i128 へ）が、`vector_pixels_q14` の入出力は変化なし。範囲の誤りは従来どおり `RasterError::Range` で、wasm が `the vector is outside the renderer's coordinate range` の `UNSUPPORTED` にする。
- 制限の位置（`pixel_aligned_world`）は変化なし。

### 設計

整数位置・等倍の制限を、「祖先が整数平行移動」に緩める。ノード自身の変形は任意の `FixedAffine` を許す。

- **配置。** `Placement { world: FixedAffine, offset: (i32, i32) }`（新規）。`offset` は祖先の変形の合成の整数平行移動（そうでなければ `UNSUPPORTED`：`a vector's ancestors must be whole-pixel translations`）。`world` は `world(self, h, id)`（祖先とノードの合成）。`pixel_aligned_world` を `placement` に作り直し、ノード自身が整数平行移動のときは従来と同じ結果（`offset` ＝全体の平行移動、ノードに変形を残す）を返す。
- **画素。** ノード自身の変形が整数平行移動でないときは新しい枝にする。範囲＝`vector_extent` の矩形の 4 隅を、ノード自身の変形で親の座標へ写した外接矩形（文書座標から `offset` を引いた局所座標。`vector_tiles` の `bounds` は局所、ウィンドウは「タイル位置 ＋ `offset`」の文書座標なので、文書座標のまま入れると `offset` が二重になる）。線幅と miter の余白は、変形の最大拡大率（`max(√(a²+b²), √(c²+d²))` の切り上げ）を掛けて足す。範囲は十分に大きくてよい（結果の画像範囲が画素を含めばよい）。`ImageBounds::validate` が `RESOURCE_LIMIT`、`budget` がタイル数の上限（`EDIT_BUDGET`）を見る。ウィンドウは従来どおり「タイル位置 ＋ `offset`」の 1:1 で、`world` をそのまま `VectorRasterizer` へ渡す。
- **確定。** 新しい枝ではノードの `transform` を恒等にする。局所座標のマスク類は、ノードの変形で写して持ち越す。`bake_relocate(node, m: Affine2)`（新規）が次を行う：`mask_linked` かつマスクありなら `mask_transform = m.concat(mask_transform)`、`vector_mask.transform` と `clip_path.transform` も同様。その後 `validate()`（失敗は `UNSUPPORTED`）。リンクされていないマスク（文書座標）は変えない。効果の選択（`Effect::selection_mut`、ノード局所の R8 で変形を持たない）が 1 つでもあれば、画素を再標本せずには移せないので `UNSUPPORTED`（`a layer with a limited effect cannot be rasterized under a transform`）。
- **範囲外。** `FixedAffine::from_affine` が失敗する変形は `INVALID_ARGUMENT`（`world` が返す既存のエラー）。`vector_pixels_q14` の `Range` は `UNSUPPORTED`。
- **グループの子。** 従来どおり整数平行移動だけを許す（グループの平坦化は対象にしない）。
- **見え方。** 100% 表示で描画と一致する（同じ `vector_pixels_q14` ＝ 描画の CPU 参照）。直接の描画と違いが出るのは 8bit への丸めだけで、不透明な内部は全バイト一致、縁は 1/255 以内（既存の試験の条件）。

### 形式と通信

- `format_version`：上げない（通常のラスターになる）。
- `PROTOCOL`：上げない（`layerEdit` の `rasterize` の挙動が広がるだけ）。要求と結果の形も変えない。
- 描画：v3 の画素は変えない。

### 作業手順

**Files:** Modify `crates/alto-engine/src/canvas.rs`、`crates/alto-wasm/src/image_versions.rs`（`layer_edit` の `Rasterizers` への切り替えは IMG-3）、`tests/e2e/image-version-tests.js`、`packages/alto-sdk/src/document.ts`（`rasterize` の注記）。

**Interfaces:** `Placement`、`Engine::placement(h, id) -> Result<Placement, EngineError>`（`pixel_aligned_world` を置き換える。private）、`bake_relocate`。

- [ ] 試験 `rotated_vector_rasterizes_at_document_resolution`（Rust）：45 度回転した長方形のパスを焼き、範囲が回転後の外接矩形を含み、内部の画素が `vector_pixels_q14` を文書座標で直接呼んだ結果と全バイト一致する。
- [ ] 試験 `scaled_vector_matches_a_vector_drawn_at_that_size`（Rust）：2 倍の変形で焼いた結果が、パスを 2 倍にして焼いた結果と、アルファ 255 の画素で全バイト一致。
- [ ] 試験 `rasterize_moves_linked_masks_with_the_transform`（Rust）：リンクされたラスターマスクとベクターマスクを持つノードを焼いても、見える領域が焼く前と同じ。
- [ ] 試験 `out_of_range_transform_is_refused`（Rust）：範囲外の変形で `INVALID_ARGUMENT`／`UNSUPPORTED`、`snapshot` 不変。
- [ ] 試験 `rasterize_refuses_a_limited_effect_under_a_transform`（Rust）：効果の選択つき＋回転で `UNSUPPORTED`。
- [ ] 試験 `rasterize_is_one_undo_and_survives_save`（Rust）：1 回の Undo で戻り、保存・再オープンの `snapshotHash` が一致。
- [ ] 既存のブラウザ試験の「拡大 2 倍で `UNSUPPORTED`」を「焼けて見た目が同じ」に直す。
- [ ] 実装、PR。

**完了条件：** 上の試験が通る。整数平行移動の既存試験（`rasterize_bakes_offsets_and_flattens_raster_groups`）が無修正で通る。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：IMG-3（`Rasterizers` への切り替えを先に入れる）。なくても進められるが、同じ関数を触るので順に入れる。
- 後に続く項目：IMG-1（`bake_relocate` を共有）。（新規）

## IMG-3 テキストのラスタライズ

### 現状

- `prepare_rasterize` は `NodeKind::Text` を `rasterize supports a raster or a group of rasters`（`UNSUPPORTED`）で拒否する。グループの子のテキストは `plain_raster` の `only raster and vector layers can be baked` で拒否する。
- v3 のテキストは、`NodeKind::Text { content, bake }` の `bake`（ハッシュ。blob に `ALTB` のバイト列）だけから描かれる。`Engine::text_bake`（`crates/alto-engine/src/text.rs`）が `(ContentHash, Vec<u8>)` を返す。書き込み時は `Engine::transact` が `bake_text_ops` で焼く。
- 画素の元：`alto_gpu::glyph_q8::glyph_bitmaps(bake, world, level, roi)`（`crates/alto-gpu/src/glyph_q8.rs`）が各グリフの被覆（8bit）を CPU で作る。GPU は `text_q14`（`crates/alto-gpu/src/renderer/tile_renderer/paint.rs`）でそれを `glyph-q14.wgsl` に重ねる。CPU 側の重ね（`source_over_q14`）の参照は `text_q14`（`crates/alto-gpu/src/cpu_composite/text_tests.rs`、試験専用）にあり、本番の CPU 関数は無い。
- 24px 未満は面積被覆のアトラス帯、以上は直接ラスタライズ（`atlas_band`）。帯の選択はレベルと拡大率に依存する。レベル 0 の帯が、100% 表示で見える画素である。
- 欠けた文字（フォントに無いグリフ）は、バイト列のプレースホルダー（`PLACEHOLDER_FONT`、1000 単位の箱）として描かれる。`TextInfo.missing` に位置が入る（`Engine::text_info`）。
- 試験：焼き込み側は `crates/alto-engine/src/text_bake_tests.rs`、GPU 一致は `crates/alto-gpu/src/cpu_composite/text_tests.rs`。ラスタライズの試験は無い。

### 11-04 からの変化

- 旧設計書は「アウトラインを `StyledPath` にして IMG-2 の経路で確定する」としていた。v3 のテキストはアウトラインのパスでなく、グリフの被覆（`glyph_bitmaps`）で描かれ、`StyledPath` の経路とは画素が違う。この案は取り下げ、同じ `glyph_bitmaps` の結果を使う。
- 旧設計書は「フォールバック枠は描かない」としていた。画面ではプレースホルダーの箱が描かれるので、同じ見え方にするため、箱も焼く。

### 設計

テキストを、v3 の描画と同じ被覆でラスターにする。

- **関数（新規）。** `crates/alto-gpu/src/glyph_q8.rs` に次を足す。
  - `text_extent(bake: &[u8], world: FixedAffine) -> Result<Option<[i64; 4]>, RasterError>`：各グリフの箱の和。被覆は作らない。描画と同じ分岐を写す：`atlas_band(..).then(|| place_q(..)).flatten()` が `Some` なら `place_q` の `dst`・`w`・`h`。`atlas_band` が偽、または `place_q` が `None`（退化した帯、`MAX_GLYPH_PX` の範囲外）なら直接帯として、有限の ROI `[-2^24, -2^24, 2^25, 2^25]` での `direct_window`（ROI との交わりを返すので、ROI より外へ出る箱は切られる）。座標の加算は `checked_add` で行い、溢れたら `RasterError::Range`、ROI に切られた結果が文書の範囲上限を超えるなら `RESOURCE_LIMIT`。
  - `text_pixels_q14(bake: &[u8], world: FixedAffine, window: [i64; 4]) -> Result<Vec<[u16; 4]>, RasterError>`：試験専用の `text_q14` と同じ手順で、`glyph_bitmaps`（レベル 0）の各切り抜きを `source_over_q14` で順に重ねる。結果は premultiplied Q14。
- **注入。** `canvas.rs` に `pub struct Rasterizers<'a> { pub vector: VectorRasterizer<'a>, pub text_extent: TextExtent<'a>, pub text: TextRasterizer<'a> }`（新規）を置き、`prepare_layer_edit` の第 3 引数を `VectorRasterizer` からこれへ替える。wasm の `layer_edit` が 3 つを作って渡す。呼び出し元は `canvas.rs` の試験（`&|_, _, _| Err("no rasterizer".into())`）と `layer_edit`。
- **処理。** `prepare_rasterize` に `NodeKind::Text` の枝を足す。`Engine::text_bake` でバイト列を得る（無ければ `UNSUPPORTED`：`the text has no bake`。画面は bake の無いテキストを透明に描くので、これは意図した差である。透明な空のラスターを黙って作らず、利用者に知らせる）。`world` は IMG-2 の `Placement`（IMG-2 より前に入る場合は従来の `pixel_aligned_world`）。範囲＝`text_extent` の矩形（`offset` を引いて局所座標へ）。タイルごとに `text_pixels_q14` を呼び、`vector_tiles` の straight への変換を共有関数 `tiles_from_q14`（新規。`vector_tiles` から切り出す）で行う。（新規）
- **結果。** ノードの種類だけを `Raster` にし、属性は IMG-2 と同じ扱い。`LayerEditResult` に `missing: usize`（`text_info` の `missing.len()`）を足す。0 のときも出す。
- **グループの子。** テキストの子は、整数平行移動なら取り込む。
- **上限。** 範囲は `MAX_IMAGE_PIXELS` と `budget`（`RESOURCE_LIMIT`）。グリフの箱の和が文書の範囲を超える極端な値は `ImageBounds::validate` で拒否される。
- **範囲外。** `glyph_map_q` が `None` を返すグリフは、描画と同じく描かない。

### 形式と通信

- `format_version`：上げない。
- `PROTOCOL`：要求は増えない。結果の `LayerEditResult` に `missing` が増えるので、IMG-3 は段階1で IMG-1 より先に入るので、上げない（SDK の型だけ増える。古い SDK は増えた欄を読まない）。
- 描画：v3 の画素は変えない。`glyph_q8.rs` の新しい関数は描画経路から呼ばない。

### 作業手順

**Files:** Modify `crates/alto-gpu/src/glyph_q8.rs`、`crates/alto-engine/src/canvas.rs`、`crates/alto-wasm/src/image_versions.rs`、`packages/alto-sdk/src/image-types.ts`、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/api/alto-engine.api.md`。

**Interfaces:** `Rasterizers`（新規）。`text_extent`、`text_pixels_q14`。

- [ ] 試験 `text_pixels_q14_matches_the_v3_reference`（`crates/alto-gpu`）：`cpu_composite/text_tests.rs` の `bake_parity_text` で作る拉丁・縦組み・プレースホルダーの 3 件で、`text_pixels_q14` と試験専用の `text_q14` が全バイト一致。
- [ ] 試験 `text_extent_contains_every_painted_pixel`：`text_pixels_q14` が非ゼロを書く画素がすべて `text_extent` の中にある。
- [ ] 試験 `text_rasterize_keeps_id_name_opacity_and_blend`（Rust）：不透明度・合成モード・ID・名前が残り、種類だけが `Raster`。
- [ ] 試験 `text_rasterize_reports_missing_characters`（Rust）：フォントに無い文字を含む文で `missing` ＞ 0、箱の画素が焼かれる。
- [ ] 試験 `text_rasterize_matches_the_canvas_pixels`（ブラウザ）：焼く前後の `pickComposite` が各チャンネル 1 以内（アルファ 255 の画素は全バイト一致）。
- [ ] 試験 `rasterize_is_one_undo_and_survives_save`（テキストの場合を足す）。
- [ ] 実装、PR。

**完了条件：** 上の試験が通る。`tests/parity/text-bake.txt` が不変。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし（IMG-2 の前に入れる）。
- 後に続く項目：IMG-2（`Rasterizers` と `tiles_from_q14` を再利用）、IMG-1（テキストの属性付き焼き込み）、SEL-3（04。同じ閉包を変えるので後に入れる（00-ROADMAP の重なりの表））。06 の焼き込みの `Result` 化（`bake_ops`、`bake_text`。持ち主は 06）とは関数を共有しないので、順序の制約は無い。TXT-1〜11 が組版を変えても、焼き込みはバイト列から作るので影響しない。（新規）

## IMG-4 コンテンツミップマップ

### 現状

- 表示タイルは GPU の表示キャッシュ（`Renderer::cache_map`、`crates/alto-gpu/src/renderer/cache.rs`）に `(level, tx, ty)` で持つ。v3 のレベル 1 以上は、レベル `level - 1` の子 4 枚の整数 2×2 平均（`downsample-q14.wgsl`）として定義される（`compose_from_children`、`downsample_cached`、`crates/alto-gpu/src/renderer/pyramid.rs`、`compose.rs`）。冷えた子は先に描いてから親を作る。
- ミップは文書にも CPU の正本にも無い。ハッシュとレベルを鍵にした派生物は存在しない。`doc/MILESTONES.md`「上限・継続課題」5 は「コンテンツミップマップは未実装」としている。
- 測定の足場：`tests/e2e/browser-tests.js` の B2（ズーム、上限 250 ms）、堅牢化 E の `stress-64mpx`（8000×8000、全タイルが異なる）。`doc/verification/ENGINE-HARDENING-RESULTS.md` E-4 で、`cold GPU first display (fit)` が 377〜380 ms、B2 が 2〜5 ms と記録されている。

### 11-04 からの変化

- 表示ピラミッドの実装は GPU キャッシュ側で残り、堅牢化 E 以降も構造は変化なし（`git log b2036ce..bdf26a9 -- crates/alto-gpu/src/renderer/cache.rs` は 725d6f4 の分割のみ）。
- 旧設計書は「B2 の改善で判定」としていたが、B2 は 2〜5 ms で、改善余地はほぼ無い。判定の指標を、`stress-64mpx` の fit のコールド表示に替える。

### 設計

**先に測る。** 実装の形は測定で決める。

1. `node scripts/verify-browser.mjs` の `stress-64mpx` の `cold GPU first display (fit)` に、`Renderer` の `profile`（`pyramid_hits`、`commands_ms`）を付けて、時間の内訳を記録する（`doc/verification/IMG4-MIPMAP-MEASURE.md`、新規）。内訳＝レベル 0 の合成、レベル 1 以上の構築、読み戻しなど。（新規）
2. PERF-1（可視タイルのバッチ化）と PERF-3（コールド描画 250 ms）が入った状態で再測定する。
3. 採否：
   - 採用する条件：PERF-1／PERF-3 の後も `stress-64mpx` の fit が 250 ms を超え、かつ超過分の過半がレベル 0 の合成（ミップで省ける部分）にある。
   - 見送る条件：上を満たさない。結果と理由を記録して終える。

**採用した場合の形。**

- 鍵：位置を含まない内容の鍵 ＝ `ContentHash::of(レベル ‖ 子 4 枚の鍵または元タイルのハッシュ（TL、TR、BL、BR の順、無い子は零のハッシュ）)`。同じ内容が別の位置にあれば同じ値を共有する。表示キャッシュの `(level, tx, ty)` は位置の鍵なので、そのまま使わない。値は `rgba8unorm` の 256×256 タイル。文書へは書かない。保存しない（キャッシュなので、無くても結果は同じ）。
- 生成：`downsample-q14.wgsl` と同じ式の 2×2 平均で、元の正本タイルの下位から作る。
- 使う条件：v3 のレベル 1 以上の定義は「子の縮小」なので、画素を変えない条件は「ミップから作った親と、子から作った親がバイト一致」である。透明・半透明レイヤーの合成順序が違うと一致しない。**単一の不透明ラスターが全面を覆うタイルに限って経路を選べる。** それ以外は従来経路にする。
- 失効：鍵が内容のハッシュなので、編集で自然に別の鍵になる。GPU 喪失では捨てて再構築する。
- 保管場所：STO-6（派生 OPFS キャッシュ）と同じ置き場にするかは、STO-1／STO-6 の設計が決まってから決める（利用者の決定ではなく実装順の問題）。既定は GPU 側のメモリのみ。
- 上限：GPU の残予算の範囲（`Renderer` の `budget_bytes`）。超えたら鍵を古い順に捨てる。

### 形式と通信

- `format_version`、`PROTOCOL`：上げない。文書にも要求にも出ない。
- 描画：画素を変えない。採用した場合は、ミップ経由と従来経路のバイト一致を `tests/parity/v3-browser.txt` の既存行で確かめる。行は足さない。

### 作業手順

**Files:** Create `doc/verification/IMG4-MIPMAP-MEASURE.md`（新規）。採用した場合に限り、Modify `crates/alto-gpu/src/renderer/pyramid.rs`、`crates/alto-gpu/src/renderer/compose.rs`、Create `crates/alto-gpu/src/renderer/content_mip.rs`（新規）。

- [ ] `stress-64mpx` の fit のコールド表示の内訳を、native と WASM で 3 回ずつ測って記録する。
- [ ] PERF-1／PERF-3 の後の値で再測定し、上記の条件で採否を書く。
- [ ] 採用の場合：試験 `content_mip_parent_equals_child_downsample`（単一の不透明ラスターで、ミップ経由と従来経路の親が全バイト一致。加えて `Renderer` の `profile` にミップの命中数を足し、ミップを使った経路でその数が 0 でないことを確かめる。使わない実装でも一致だけは通るため）、`content_mip_is_not_used_for_translucent_stacks`（半透明の重なりでは従来経路になる）、`gpu_loss_drops_the_content_mips`。実装、PR。
- [ ] 見送りの場合：測定文書だけをコミットし、ROADMAP の IMG-4 を「見送り」に更新する。

**完了条件：** 測定文書がある。採用なら上の試験が通り、既存の parity が不変。

**検証：** `node scripts/verify-browser.mjs`（ハードウェア依存の B1〜B7 を含む）。採用の場合は `sh scripts/check.sh native` も。

### 依存

- 先に要る項目：STO-1（部分読み込みがタイルの取得経路を変えるため、鍵を決める前に入れる）、PERF-1、PERF-3（再測定の前提）。
- 後に続く項目：なし。STO-6（派生 OPFS キャッシュ）が保管場所で関わる。

## IMG-5 画像編集の GPU 高速化

### 現状

- 画素編集は CPU が正本で、`Engine::prepare_image_edit`（`crates/alto-engine/src/image/mod.rs`）が同期で全画素を計算し、`stage_tile`（`crates/alto-engine/src/lib.rs`）でタイルを LZ4 とハッシュで保管する。リサイズは `resize_pixel`（`crates/alto-engine/src/image/resample.rs`）の整数フィルタ（nearest、bilinear、Catmull-Rom、area）。`Gradient` は画素ごとの `gradient`、`Fill` は走査線の塗りつぶしで逐次処理である。
- ベンチ：`crates/alto-engine/examples/image_bench.rs`（`cargo run --release -p alto-engine --example image_bench`）が 4096² の flip、rotate、resize の 4 つのフィルタ、gradient の所要を測る。`prepare_image_edit` 全体の時間で、計算と `stage_tile` の内訳は無い。`fill` は含まない。
- GPU で画素編集をする部品は無い。GPU の読み戻しは非同期（`readTiles`）で、`image_edit` は同期の wasm 関数である。
- 試験：`crates/alto-engine/src/image/tests.rs`、`crates/alto-engine/src/image/reference_validation_tests.rs`（整数フィルタの参照）。

### 11-04 からの変化

- 旧設計書の image.rs は `image/` ディレクトリに分割された（126f2b5）。`resize_pixel` は `image/resample.rs`、`Reader`/`Writer` は `image/pixels.rs`。
- ベンチは既にある（`image_bench.rs`）。旧設計書は「ベンチで測る」とだけ書いていたが、足りないのは計算と符号化の内訳と `fill` である。
- 判断（読み戻しと符号化が残るので、測定が先）は変化なし。

### 設計

**先に測る。**

1. `image_bench.rs` を拡張する：各操作で `prepare` を「計算（`stage_tile` の前）」と「`stage_tile`（LZ4・ハッシュ）」に分けて出す。`fill` を足す。WASM でも同じ操作を測る（`tests/e2e/perf-tests.js` に足す）。
2. 判定：符号化（`stage_tile`）の比率が 50% 未満の操作だけを GPU 化の候補にする。それ以外は CPU のまま。`fill` は逐次の走査で GPU に向かないので、測定の結果によらず候補から外す。
3. 結果を `doc/verification/IMG5-IMAGE-EDIT-MEASURE.md`（新規）に記録する。候補が 0 なら、GPU 化を見送ると書いて終える。

**候補が残った場合の形。**

- CPU の結果が正本である。GPU 経路はその加速で、一致しない入力は CPU 経路に落とす。
- 非同期にする必要がある（読み戻しは `await`）。IMG-1 の「計画（同期）・合成（非同期）・確定（同期）」の機構を再利用し、`imageEditGpu`（新規）として別の要求にする。同期の `imageEdit` は変えない。（新規）
- シェーダー：`crates/alto-gpu/shaders/image-edit-q14.wgsl`（新規）。モード番号は既存のシェーダーと別なので、効果の番号（34〜43。02）とは無関係。
- 整数の定義域：WGSL は 64 ビット整数を持たない。`resize_pixel` は座標の計算を i128 で行う（`origin * 256 * 2 * new + …`）。GPU 経路は、すべての中間値が i32／u32 に収まる入力（辺長と原点の積が `2^31 / 512` 未満）に限り、それ以外は CPU に落とす。
- GPU 喪失・予算超過・`GPU_UNSUPPORTED`・定義域外は、エラーでなく CPU への切り替え。文書は変わらない。
- 3 つの GPU（Metal、lavapipe、SwiftShader）で CPU と全バイト一致しなければ採用しない。

### 形式と通信

- `format_version`：上げない。
- `PROTOCOL`：測定で採用が決まり、新要求を足す場合は上げる（14。段階4の回）。見送りなら上げない。
- 描画：v3 の画素は変えない。結果は CPU と同じ画素で、`tests/parity/` に行は足さない。

### 作業手順

**Files:** Modify `crates/alto-engine/examples/image_bench.rs`、`tests/e2e/perf-tests.js`。Create `doc/verification/IMG5-IMAGE-EDIT-MEASURE.md`（新規）。判定を通った操作がある場合に限り、Create `crates/alto-gpu/shaders/image-edit-q14.wgsl`（新規）、Create `crates/alto-gpu/src/renderer/image_edit.rs`（新規）、Modify `crates/alto-wasm/src/image_versions.rs`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/document.ts`。

- [ ] `image_bench.rs` に内訳と `fill` を足し、4096² で `resize`（4 つのフィルタ）、`gradient`、`fill` を native と WASM で測る。
- [ ] 判定条件で対象を決めて記録する。候補が無ければここで終える。
- [ ] 対象の操作の試験 `gpu_edit_matches_the_cpu_bytes`（3 つの GPU で全バイト一致。一致しない入力は CPU 経路へ落ちることも確かめる）。
- [ ] 試験 `GPU loss during an accelerated edit falls back to the CPU`：読み戻しの失敗を注入しても、結果が CPU の結果と一致し、文書が壊れない。
- [ ] 試験 `accelerated_edit_is_one_undo_and_survives_save`。
- [ ] 実装、PR。

**完了条件：** 測定文書がある。採用なら上の試験が通り、CPU の既存の試験（`reference_validation_tests.rs`）が無修正で通る。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。測定は `node scripts/verify-browser.mjs --suite=perf`（`--suite=` に名前を渡す。`scripts/verify-browser.mjs` の先頭の説明を確かめる）。

### 依存

- 先に要る項目：IMG-1（非同期の準備・確定の機構）。
- 後に続く項目：IMG-6（`warp` を GPU 化する場合の土台。ただし IMG-6 は CPU の実装が先）。

## IMG-6 遠近法、メッシュ変形、液化

### 現状

- 幾何の画素編集は `ImageEdit`（`crates/alto-engine/src/image/mod.rs`）の `Crop`、`Resize`、`Rotate`（90 度の倍数のみ）、`Flip` だけ。`Resize` は `resize_pixel` の整数フィルタ、`Rotate` と `Flip` は座標の入れ替えである。任意の逆写像でサンプルする部品は無い。
- 逆写像の標本化に使える部品：`resize_pixel` の bilinear（`Q8` の座標 `qx`、`qy` から 4 近傍を `fx`、`fy`（0〜255）で重み付けし、Q14 の premultiplied で足して straight に戻す）と、`catmull_rom_pixel`（`crates/alto-engine/src/image/resample.rs`）。範囲外の近傍は画像の端へクランプする。
- 選択の扱い：`prepare_image_inner` の `selected`／`mask`／`selection(self, mask, m, x, y)`（`crates/alto-engine/src/image/selection.rs`）が、`Gradient` などで画素ごとの被覆（0〜255）を返す。`EMPTY_SELECTION` は選択が空のときに返す。
- SDK：`doc.image.resize`／`rotate`／`flip`／`crop`／`gradient`／`fill` など（`packages/alto-sdk/src/document.ts`）。いずれも `imageEdit`（JSON）を使う。意図の名前は `ImageEdit::intent`（`image.crop` など）。
- 変位場のような大きなバイナリを渡す要求は、`provideObjects` や `import`（`ArrayBuffer`、転送つき）の形がある（`packages/alto-sdk/src/protocol.ts`）。

### 11-04 からの変化

- 旧 image.rs は `image/` に分割され、`resize_pixel` は `image/resample.rs`、`Reader`/`Writer` は `image/pixels.rs`、選択は `image/selection.rs` に移った（126f2b5）。`warp.rs` は `image/` の下に置く（新規）。
- `ImageFilter` に `CatmullRom` と `Area` が増えた（11-04 より前から存在するが、旧設計書は `bilinear` しか触れていない）。
- 旧設計書は「本機能は元の設計で対象外だったが、00 の D1 で対象にした」と書いていた。方針は変化なし（実装する）。

### 設計

Affine2 の非破壊変形では表せないため、すべて確定編集とする（レイヤーの属性を増やさない）。`ImageEdit` に `Warp` を足す。

```rust
// crates/alto-engine/src/image/mod.rs（ImageEdit の新しい枝）
Warp {
    node: NodeId,
    #[serde(flatten)] kind: WarpKind,
    #[serde(default)] filter: ImageFilter,           // Nearest / Bilinear / CatmullRom。Area は INVALID_ARGUMENT
    #[serde(default)] bounds: Option<ImageBounds>,    // 出力範囲。既定は元の範囲
    #[serde(default)] selection: SelectionMode,
}
pub enum WarpKind {
    Perspective { corners: [[f64; 2]; 4] },                  // 元の範囲の左上・右上・右下・左下の行き先（局所座標）
    Mesh { cols: u32, rows: u32, points: Vec<[f64; 2]> },    // (cols + 1) × (rows + 1) 個の行き先
    Displace { region: ImageBounds, width: u32, height: u32 }, // 変位場の画素は serde に載せず、準備関数の別引数で渡す
}
```

```ts
doc.image.warp(node, { kind: "perspective", corners: [[x, y], ...4] }, { filter?: ImageFilter; bounds?: ImageBounds; selection?: ImageSelection })
doc.image.warp(node, { kind: "mesh", cols, rows, points: [[x, y], ...] }, options)
doc.image.warp(node, { kind: "displace", field: Int16Array /* Q4 の dx, dy を画素ごとに並べる */, region: ImageBounds }, options)
```

- **逆写像。** すべて「出力画素 → 入力座標」の整数写像で、`resize_pixel` と同じ固定小数のフィルタを使う。出力画素の中心 `(2x + 1) / 2` に対する入力の標本位置を Q8（1/256 画素）で求め、`resize_pixel` の bilinear と同じ重み（`fx`、`fy`）で合成する。`resize_pixel` の標本部分を `sample_q8(reader, b, qx, qy, filter)`（新規。`image/resample.rs`）に切り出し、`resize_pixel` がそれを呼ぶ形に直す（既存の出力は 1 バイトも変えない）。
- **範囲外。** 標本位置が元の範囲 `[b.x, b.right()) × [b.y, b.bottom())` の外なら透明 `[0; 4]`。範囲内の 4 近傍が外へ出る分は、`resize_pixel` と同じく端へクランプする。
- **遠近。** 4 隅の行き先を Q8 の整数に丸め（`round`）、8 点から 3×3 のホモグラフィを i128 の有理数（共通の分母）で解く。行き先の 4 点が凸でない、3 点が一直線、または行列式が 0 のときは `INVALID_ARGUMENT`（`singular perspective`）。標本位置 `(u, v) = (a·x + b·y + c, d·x + e·y + f) / (g·x + h·y + 1)` の除算は i128 の丸め除算（`resize_pixel` の `rounded` と同じ規則）。分母が 0 以下になる画素は透明。
- **メッシュ。** 元の範囲を `cols × rows` の格子に等分し、各格子点の行き先を `points` で与える（行優先）。各セルを 2 つの三角形に割り、三角形ごとに整数の重心座標で逆写像する（Q16）。三角形の走査は上と左の辺を含め、右と下の辺を含めない。重なる三角形は後のセルが勝つ。折り返したメッシュも許す（検査しない）。
- **液化。** `Displace`：出力画素 `(x, y)` の標本位置 ＝ `(x * 256 + dx * 16, y * 256 + dy * 16)`（Q8）。`dx`、`dy` は変位場の Q4（1/16 画素）の `Int16`。`field` は `region` の画素ごとに `[dx, dy]` を行優先に並べる（長さ `width * height * 2`）。`region` の外は恒等。変位場を作る液化ブラシは SDK 側（`packages/alto-sdk/src/liquify.ts`、新規）：`LiquifyField`（`push`、`pinch`、`twirl`）が `Int16Array` を累積する。浮動小数を使ってよい（変位場は入力データで、文書の画素の計算には入らない）。文書には変位場を保存しない。（新規）
- **選択。** `selection` が `Current`（既定）なら、`selection(...)` の被覆（0〜255）で結果と元の画素を `lerp_premultiplied`（`image/stroke.rs`）で混ぜる。選択が空なら `EMPTY_SELECTION`。被覆が全画素で 0 のときも `EMPTY_SELECTION`（`Gradient` と同じ）。
- **出力範囲。** 既定は元の範囲。`bounds` で広げられる（遠近で外へ出る場合）。`ImageBounds::validate` と `MAX_IMAGE_PIXELS`、作業量は `EDIT_BUDGET`（変位場の大きさ `width * height * 4` バイトと入力・出力のタイル）で `RESOURCE_LIMIT`。
- **上限。** メッシュ 64×64（`cols`、`rows` ＞ 64 は `INVALID_ARGUMENT`）、点の数は `(cols + 1) * (rows + 1)` と一致、座標は有限で絶対値が `i32::MAX` 以下。変位場は辺 4096 以下（`MAX_WARP_FIELD_SIDE = 4096`、新規）、`dx`、`dy` は `Int16`。長さが合わなければ `INVALID_ARGUMENT`。
- **属性。** ノードの変形・マスク・効果は変えない（`geometry` は偽で、`Gradient` と同じく画素だけを替える）。範囲を広げたときも変形は変えない。
- **エラー。** 退化した入力は `INVALID_ARGUMENT`、範囲・作業量は `RESOURCE_LIMIT`、ラスターでないノードは `INVALID_ARGUMENT`（`raster required`）、ロックは `INVALID_ARGUMENT`。
- **意図の名前。** `image.warp`（`BuiltInIntent`、`packages/alto-sdk/src/intents.ts` に足す）。

### 形式と通信

- `format_version`：上げない（結果は通常のラスター）。
- `PROTOCOL`：上げる。変位場の転送に新要求 `imageWarp: Q<{ edit: Record<string, unknown>; field?: ArrayBuffer }, { state: WireState; result: ImageEditResult }>` を足す。遠近とメッシュも同じ要求で通し、`imageEdit` の JSON に変位場を載せない。値は 14（段階4の回）。
- 描画：v3 の画素は変えない。CPU の確定編集のみ。

### 作業手順

**Files:** Create `crates/alto-engine/src/image/warp.rs`（新規）、`packages/alto-sdk/src/liquify.ts`（新規）。Modify `crates/alto-engine/src/image/mod.rs`（`ImageEdit::Warp`、`intent`）、`crates/alto-engine/src/image/resample.rs`（`sample_q8`）、`crates/alto-wasm/src/image_versions.rs`（`image_warp`）、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/image-types.ts`、`packages/alto-sdk/src/intents.ts`、`packages/alto-sdk/api/alto-engine.api.md`。

**Interfaces:** `ImageEdit::Warp`、`WarpKind`、`sample_q8`、`doc.image.warp`。`Engine::prepare_image_edit_with(h, edit, aux: Option<&[i16]>)`（新規。`prepare_image_edit` は `aux` を `None` にして呼ぶ）が変位場を受け取る。wasm の `image_warp(request, field: &[u8])`（新規）が `Int16Array` のバイト列を `i16` に直して渡す。

- [ ] 試験 `sample_q8_keeps_resize_bytes`：`sample_q8` へ切り出した後も、`image_bench.rs` の resize 4 種と既存の `reference_validation_tests.rs` が全バイト同じ。
- [ ] 試験 `identity_perspective_is_identity`、`identity_mesh_is_identity`、`zero_displacement_is_identity`（Nearest と Bilinear で、出力が入力と全バイト一致）。
- [ ] 試験 `perspective_maps_the_corners`：4 隅の行き先が整数のとき、行き先の画素が元の隅の画素の値を持つ。
- [ ] 試験 `singular_perspective_is_refused`：3 点が一直線、凸でない、行列式 0 で `INVALID_ARGUMENT`、`snapshot` 不変。
- [ ] 試験 `mesh_limits_and_point_count_are_checked`：65×65、点の数の不一致、非有限で `INVALID_ARGUMENT`。
- [ ] 試験 `displacement_field_length_and_side_are_checked`：長さ不一致、辺 4097 で `INVALID_ARGUMENT`。
- [ ] 試験 `warp_selection_limits_the_change`：選択の外の画素が変わらず、空の選択が `EMPTY_SELECTION`。
- [ ] 試験 `warp_with_bounds_grows_the_layer_within_limits`：`bounds` で広げた結果が範囲を含み、`MAX_IMAGE_PIXELS` 超過が `RESOURCE_LIMIT`。
- [ ] 試験 `warp_is_one_undo_and_survives_save`（Rust、ブラウザ）：1 回の Undo で戻り、保存・再オープンの `snapshotHash` が一致。
- [ ] 試験 `liquify_field_is_clamped_and_deterministic`（SDK）：同じ入力列で同じ `Int16Array`、範囲外は端に丸める。
- [ ] ブラウザ試験 `warp through the SDK`：3 種の `warp` が `imageWarp` を通って結果を返し、変位場が転送される。
- [ ] 実装、PR。

**完了条件：** 上の試験が通る。既存の `Resize`／`Rotate`／`Flip` の出力が不変。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし。
- 後に続く項目：IMG-5（`warp` を GPU 化する場合）。

## Review Focus

- 焼き込みで、読み戻しの間に別の編集が走る場合：Worker のキューが直列で SDK からは起きないこと、確定の `base_revision` 検査が残ること。
- 効果がノードの範囲外へ広がるときに切れないこと（`region` が効果の出力範囲を含む）。
- 非表示の親グループの中のレイヤーの焼き込み。親の不透明度・合成モード・マスク・効果は焼かないことを SDK の文書コメントに明記する。
- 透明な範囲を持つ `ImageBounds` が、焼き込み後に正しいこと（外接矩形へ縮める）。
- 8bit の量子化で、焼いた後の見た目が Q14 の中間より 1/255 ずれうること。効果の連鎖の途中では一致しない。この差を試験の条件に明記する。
- 表示キャッシュ `cache_map` がシーンを区別しないこと。一時シーンの読み戻しが実画面のタイルを残したまま壊さない。
- 任意変形のラスタライズで、リンクされたマスク・ベクターマスク・クリップパスの変形を移し替えること。効果の選択つきは拒否すること。
- `Rasterizers` への切り替えで、`canvas.rs` の既存の試験と `layer_edit` が全部直っていること。
