# ブラシ

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 丸ブラシ以外の先端と、傾き・速度・カーブによる動的な変化を追加し、Undo の深さを設定可能にする。

**Architecture:** ストロークは `StrokeSampler`（`crates/alto-raster/src/brush.rs`）が入力サンプルからダブ列を作り、GPU（`crates/alto-gpu/shaders/brush.wgsl`）が f32 でスタンプする。終了時に読み戻した画素がタイルの正本で、履歴は結果（`Op::SetTiles`）で持つ。GPU 間の差は文書へ混ざらない。ダブ生成は純粋関数で、入力が同じなら同じダブ列になる。

**ブラシは v3 の整数経路ではない。** 11-04 版は `stroke_q8.rs` を「v3 のストロークのカバレッジ」と書いたが誤りで、`crates/alto-gpu/src/stroke_q8.rs` はベクターパスの線の輪郭（`alto_vector::Stroke`）を整数で作る。ブラシは通らない。ブラシの検証は、WGSL と Rust の `cpu_paint` を ±2 の許容で比べる試験（`tests/e2e/browser-tests.js` の `GPU brush / pressure / tile boundary / exact Undo-Redo / eraser`）で行う。`tests/parity/*.txt` にブラシの行はない。

**共通規則2の例外。** ROADMAP 共通規則2（描画の3実装、全バイト一致、v3 のシェーダーに浮動小数の演算を入れない）は、ブラシのストローク（`brush.wgsl`、f32）には適用しない。理由：ストロークの画素は GPU のスタンプ結果を読み戻して `Op::SetTiles` の正本にするので、GPU 間の差は文書へ混ざらず、履歴は結果で持つ。範囲：`brush.wgsl` と `cpu_paint`（ストロークの確定タイル）だけ。表示の合成（`effects-q14.wgsl` など）と、`tests/e2e/cpu-composite.js` を通る v3 の画素は例外に含めない。この例外は 00-ROADMAP の共通規則2に注記として記録した。

## 決定が要る点

| 点 | 案 | 既定 |
|---|---|---|
| 画像先端・テクスチャ画像を文書に保存するか | A：保存しない。SDK が画素列を持ち、ストロークの開始時に渡す。B：`Op::SetAsset` で文書のアセットにする（`format_version` を上げ、GC の根と使用中の削除拒否が要る） | A |
| `pressureSize` / `pressureOpacity` の扱い | 削除して `dynamics` へ統合する。SDK の公開 API の破壊的変更になる（`packages/alto-sdk/api/alto-engine.api.md` に載っている） | 削除 |
| パターンの座標系 | レイヤー座標に固定する。文書座標にするにはシェーダーへワールド行列が要る | レイヤー座標 |
| Undo のバイト上限 | `Vec<Transaction>` のバイト数に上限を足す（BR-6）。足さないと1000ステップで線形に増える | 足さない（測ってから） |
| 速度の正規化 | `min(v / 4, 1)`（レイヤー画素 / ms）。ズームに依らず、画面上の速さとは一致しない | 4 px/ms |

## BR-1 先端の角度と丸み

### 現状

- `crates/alto-raster/src/brush.rs`：`BrushConfig { size, hardness, spacing, opacity, flow, pressure_size, pressure_opacity }`（`Serialize, Deserialize`、`deny_unknown_fields` なし）。`Dab { center, radius, opacity, angle }` の `angle` は `StrokeSampler::dab` が常に 0 を入れ、どこでも読まない。`cpu_paint(base, origin, dabs, config, color, erase)` が CPU の参照で、円の距離だけを見る。
- `crates/alto-gpu/shaders/brush.wgsl`：`struct Dab { center_radius: vec4f, extra: vec4f }`。距離は `distance(p, d.center_radius.xy) / max(0.01, d.center_radius.z)`、形は `1 - smoothstep(config.params.x, 1.0, dist)`。`extra` は未使用。
- `crates/alto-gpu/src/renderer.rs`：`GpuDab { center_radius: [f32; 4], extra: [f32; 4] }`（`extra` は `[0.0; 4]`）と `BrushUniform { color, params, origin, info }`。`crates/alto-gpu/src/renderer/stroke.rs` の `Renderer::stamp` が、ダブの外接円（`center ± radius`）でタイルを選ぶ。
- `crates/alto-wasm/src/lib.rs`：`Runtime::begin_brush` が `BrushConfig` を JSON で受け、`size`・`hardness`・`spacing`・`opacity`・`flow` のどれかが有限でない、または `size` が 0 以下か 2048 超なら `INVALID_ARGUMENT: brush size`。そのあと `hardness` を 0〜0.999、`spacing` を 0.01〜1、`flow` と `opacity` を 0〜1 へ丸める。`Runtime::cpu_brush_reference`（js_name `cpuBrushReference`）が試験用に `cpu_paint` を呼ぶ。
- `packages/alto-sdk/src/tools.ts`：`BrushOptions.tip: { size?, hardness?, spacing? }`。`ToolSession` のコンストラクターが `beginBrush` に `config` を渡す。`packages/alto-sdk/src/protocol.ts` の `BrushConfig` が配線上の型。

### 11-04 からの変化

- ブラシ本体（`brush.rs`、`brush.wgsl`、`tools.ts`）は変化なし。
- `renderer.rs` の分割（`725d6f4`）でスタンプは `crates/alto-gpu/src/renderer/stroke.rs` へ移った。`Editor` の置き場も同様に動いた（BR-6）。
- 11-04 版の「`stroke_q8.rs` が v3 のストロークのカバレッジ」は誤りで、本版で直した（冒頭）。

### 設計

ダブの形を楕円にする。`radius` は長軸の半径で、短軸の半径は `radius × roundness`。

- `BrushConfig` に `angle: f32`（度、既定 0、`serde(default)`）と `roundness: f32`（既定 1、`serde(default)`）を足す。`begin_brush` で `angle` は有限であること（違えば `INVALID_ARGUMENT: brush angle`）、`roundness` は `[0.01, 1]` へ丸める（`hardness` などの既存の丸め方に合わせる）。`angle` は `[-360, 360]` を `rem_euclid(360)` で正規化する。
- `Dab` に `roundness: f32` を足す。`angle` は度から**ラジアン**へ変えてダブに入れる。角度はレイヤー座標（`begin_brush` が `input_inverse` で入力をレイヤー座標にしたあとの座標）で測る。回転した表示やレイヤー変形の下でも、描いたレイヤーの中で同じ向きになる。
- ホストで `cos`・`sin` を計算し、`GpuDab.extra = [cos, sin, roundness, 0]` に入れる。シェーダーに三角関数を入れない。
- `brush.wgsl`：`roundness >= 1.0`（`extra.z >= 1.0`）のときは今の `distance(...)` をそのまま使う。丸いダブは円で、回転しても形が変わらないので、**角度は見た目に効かない**（BR-4 の傾きも `roundness < 1` か画像先端のときだけ見える。矛盾ではなく仕様）。この分岐で既存の丸いダブが従来と同じ式を通るため、出力は変わらない（同じ GPU の上で試験する）。それ以外は `q = p - center`、`u = q.x*cos + q.y*sin`、`v = -q.x*sin + q.y*cos`、`dist = length(vec2f(u, v / max(roundness, 0.01))) / max(0.01, radius)`。`roundness` は 0.01 以上に丸め済みでゼロ除算は起きない。
- `cpu_paint` も同じ式にする（分岐も同じ）。
- タイルの選択（`Renderer::stamp`）は外接円のまま。楕円は外接円に収まるので変えない。

SDK：`BrushOptions.tip` に `angle?: number`（度、既定 0）と `roundness?: number`（0.01〜1、既定 1）。

### 形式と通信

- `format_version`：触れない。ストロークは `Op::SetTiles` で保存され、ダブは保存されない。
- render v3 の画素：触れない（表示の合成は変えない）。既存の丸いダブは従来と同じ式を通る。
- `PROTOCOL`（`packages/alto-sdk/src/protocol.ts`、現在 9）：`beginBrush` の `config` が増えるので上げる（10。00-ROADMAP の番号の表の段階2の回）。段階1のプロトコルは BR-6 で 9 に更新済み。BR-1〜BR-3 は段階2の回（10）で、同じ回の2本目以降は上げない。README と RELEASING を同じ PR で更新する。
- シェーダー：`brush.wgsl` の既存モードの変更で、新しいモード番号は不要。

### 作業手順

**Files:**
- Modify `crates/alto-raster/src/brush.rs`（`BrushConfig.angle` / `roundness`、`BrushConfig::normalized()`（新規）、`Dab.roundness`、`cpu_paint`）。
- Modify `crates/alto-gpu/shaders/brush.wgsl`、`crates/alto-gpu/src/renderer/stroke.rs`（`GpuDab.extra` の充填）。
- Modify `crates/alto-wasm/src/lib.rs`（`begin_brush` の検証）、`packages/alto-sdk/src/tools.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`。
- Modify `tests/e2e/browser-tests.js`。

**試験：**
- [ ] `brush.rs`：`round_dab_with_roundness_one_is_unchanged`。`roundness = 1`（`angle` は任意）の `cpu_paint` の出力が、`roundness` 導入前の式の出力と全バイト一致する（期待値は変更前のコードで生成してテストに埋める）。
- [ ] `brush.rs`：`config_normalization_clamps_roundness_and_wraps_angle`。`BrushConfig::normalized()`（新規。`begin_brush` が呼ぶ検証と丸め。非有限の `angle` は `Err`）で、`roundness = 0` が 0.01、`angle = -90` と `270` が同じ値になる。
- [ ] `brush.rs`：`thin_ellipse_has_no_division_by_zero`。`roundness = 0.01` で `cpu_paint` の画素が有限で、中心の画素が覆われる。
- [ ] `browser-tests.js`：`an elliptical brush is wider along its angle`。`angle` 0 と 90 の楕円で、横に 3 点・縦に 3 点の画素幅が入れ替わる。`cpuReference` との差が ±2 以内。
- [ ] `browser-tests.js`：`a round dab ignores its angle on the GPU`。`roundness: 1` で `angle: 0` と `angle: 45` のストロークを同じ GPU で描き、`readTile` の全バイトが一致する（この試験は分岐が外れると失敗する）。
- [ ] `browser-tests.js`：既存の `GPU brush / pressure / tile boundary / exact Undo-Redo / eraser` が変更なしで通る（`config` の `pressure_size` は BR-5 で `dynamics` に直す）。

**完了条件：** 上の試験が通り、既存の丸いブラシの画素が変わらない。

**検証：** `sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る項目：なし。
- 後に続く項目：BR-2（楕円の変形を画像先端が共有する）、BR-4（`angle` を傾きから決める）。COL-2（01。`brush.wgsl` の 16bit 版は BR-1〜BR-3 の後に足す（00-ROADMAP の重なりの表））。

## BR-2 画像の先端

### 現状

- `crates/alto-doc/src/node.rs`：`AssetKind` に `BrushTip` と `Pattern`（どちらも寸法なしの単位バリアント）がある。`crates/alto-doc/src/version.rs` が文字列 `"brush_tip"` / `"pattern"` にするだけで、読み込み・保存・描画の経路はどこにもない。`AssetRecord { id, hash, kind, byte_len, dims }`。
- `packages/alto-sdk/src/engine.ts`：`engine.assets.importImage(file)`（`createImageBitmap` と `OffscreenCanvas` で画素を取り、Worker の `import` へ送る）、`importFont`、`releaseImport`。
- `Renderer::stamp`（`crates/alto-gpu/src/renderer/stroke.rs`）は、**タイルが初めて触れられたとき**に、タイルごとの `stroke base`・`stroke output`（`texture` 関数、`Rgba8Unorm` 256×256、`Class::Stroke`）、`coverage` バッファ 262144 バイト、選択があれば選択テクスチャを確保する。見積もりは `tile_bytes = 786432 + (選択あり 262144)` と `command_bytes`。`new_bytes` が `budget_bytes` を、`cpu_finish` が 512 MiB を超えると、確保の前に `RenderError::Budget`（コード `INVALID_ARGUMENT`）で拒否し、ストロークは続けられる。`begin_stroke` は `GpuStroke`（`crates/alto-gpu/src/renderer.rs`）の欄を埋めるだけで確保しない。
- GPU を失ったストロークは `GPU_LOST` で終わり、文書はストローク前のまま（`doc/verification/ENGINE-HARDENING-RESULTS.md` の `gpu-stroke` の行）。
- バインドグループは binding 0〜5（`base`、`output`、`coverage`、`dabs`、`config`、`selection`）。`selection` が無いときは `tile.base` のビューを代わりに束ねている。

### 11-04 からの変化

- 変化なし（`AssetKind` は `126f2b5` で `lib.rs` から `node.rs` へ移っただけ）。
- 11-04 版は「`AssetKind::BrushTip` を `BrushTip { width, height }` に変えて GC の根にする」としたが、本版は既定でそれをしない（決定が要る点の A）。ストロークの結果はタイルで、先端の画像は結果に残らないため。「GC の根」と「使用中の削除拒否」は、案 B を選んだ場合のみ要る。

### 設計

R8 の先端画像を、ストロークの開始時に渡して使う（案 A）。

**画像の型。** SDK の `BrushImage { width: number; height: number; pixels: Uint8Array }`（新規）。`pixels` は R8、`width × height` バイト。辺は 16〜1024（非正方形可）。

**取り込み。** `engine.assets.importBrushTip(file: Blob): Promise<BrushImage>`（新規）。`importImage` と同様に `createImageBitmap` と `OffscreenCanvas` で RGBA を取り、整数だけで R8 へ変換する。

`luma = (54 r + 183 g + 19 b + 128) >> 8`、`coverage = (luma * a + 127) / 255`（整数除算）。白く不透明な画素が全カバレッジ、透明と黒が 0。辺が 16 未満か 1024 超なら `INVALID_ARGUMENT`（`importBrushTip` は拒否し、状態を変えない）。エンジンは何も保持しないので、`releaseImport` も GC も要らない。（新規）

**ストロークへ渡す。** `BrushOptions.tip.image?: BrushImage`。`ToolSession` が `beginBrush` へ `tipImage: { width, height, pixels: ArrayBuffer } | null` を足して送る（`pixels` は転送）。Worker は `runtime.beginBrush` の引数に足す。`begin_brush` は `pixels.len() == width * height`、辺が 16〜1024 を検証する（違えば `INVALID_ARGUMENT: brush tip`、ストロークは始まらない）。

**描画。**
- `BrushConfig` には画像を持たせない。`Renderer::begin_stroke` に `tip: Option<BrushImage>`（Rust 側の型は `alto_raster` の `BrushImage`（新規））を足し、`GpuStroke` には CPU 側の画素列を `tip: Option<BrushImage>` で預け、テクスチャ用の欄 `tip_texture`（新規）は空にしておく。`begin_stroke` は確保しない。
- **確保と予算の検査は `stamp` に置く。** 最初の（ダブが空でない）`stamp` で `tip_texture`（新規）が `None` なら、タイルのループの前に作る。`Rgba8Unorm`（R を値、他を 255。`rgba_pixels` の R8 変換と同じ）、`memory::create_texture`（`Class::Stroke`）、最大 1024×1024×4 = 4 MiB。その大きさを `new_bytes` に足してから既存の予算検査（`budget_used + new_bytes > budget_bytes`）を通す。超えるなら確保せず `RenderError::Budget`（ストロークは続けられる）。CPU の読み戻し予約（`cpu_finish`）には影響しない。解放は、タイルの `StrokeTile` と同じ場所：`cancel_stroke` と、確定後の解放。GPU 喪失のときはストロークごと `GPU_LOST` で終わる。
- バインドグループに binding 6（先端）を足す。無いときは `tile.base` のビューを束ね、`config.info.w` のビット0 で使用を示す。
- `Dab.size` に当たるのは `radius` で、画像の**長辺が直径 `2 × radius`**。短辺は縦横比を保つ。`roundness` と `angle` は BR-1 の変形をそのまま使う（`u` / `v` を求めたあとに画像座標へ写す）。`s = 2·radius / max(w, h)`、`tx = u / s + w/2`、`ty = (v / roundness) / s + h/2`。
- サンプルは `textureLoad` 4 点の手書きの双線形（texel 中心は `t - 0.5`、範囲外の texel は 0）。ハードウェアのサンプラーを使わないので GPU 間の差が出ない。形は `shape = tip(tx, ty)`（`hardness` は無視）。`v = 1 - (1 - v)(1 - shape × d.opacity)` は既存のまま。
- 画像先端のダブの外接半径は `radius × 1.4143`（√2）。`Renderer::stamp` のタイル選択と `bounds` / ダメージの計算でこの半径を使う（長方形が回転したときの対角を覆う）。
- `cpu_paint` に `tip: Option<&BrushImage>` を足し、同じ式でサンプルする。

**注意。** ストローク中のプレビューと確定結果は同じシェーダーの出力で、確定は読み戻しの画素（`begin_finish_stroke`）を正本にする。ブラシは整数経路ではないので、CPU との一致は ±2 の許容で見る（冒頭）。手書きの双線形で ±2 を超えるなら、最近傍（`round(t)`）へ落とす。その判断は測って決め、速度の約束は書かない。

**上限：** 辺 16〜1024、画素列は `width × height` バイト。1 ストロークに 1 つ。

### 形式と通信

- `format_version`：案 A は触れない。案 B は `AssetKind` が `deny_unknown_fields` の型なので、`BrushTip { width, height }` への変更で上げる（案 B を選んだときだけ。そのとき作業中の回の番号に入れ、回が無ければ次の空き番号を取る。00-ROADMAP の番号の表）。
- render v3：触れない。
- `PROTOCOL`：`beginBrush` の要求に `tipImage` が増えるので上げる（10。BR-1 と同じ段階2の回）。`packages/alto-sdk/api/alto-engine.api.md` に `BrushImage` と `importBrushTip` を足す。（新規）
- シェーダー：`brush.wgsl` の既存モードの変更。

### 作業手順

**Files:**
- Modify `crates/alto-raster/src/brush.rs`（`BrushImage`（新規）、`cpu_paint`）。
- Modify `crates/alto-gpu/shaders/brush.wgsl`、`crates/alto-gpu/src/renderer.rs`（`GpuStroke` の欄、束ねる binding の追加）、`crates/alto-gpu/src/renderer/stroke.rs`（`begin_stroke`、`stamp` の予算検査と確保、`cancel_stroke` の解放）、`crates/alto-gpu/src/renderer/readback.rs`（確定後の解放が要る場合）。
- Modify `crates/alto-wasm/src/lib.rs`（`begin_brush`）、`packages/alto-sdk/src/worker.ts`、`protocol.ts`、`tools.ts`、`engine.ts`（`importBrushTip`）、`packages/alto-sdk/api/alto-engine.api.md`、`tests/e2e/browser-tests.js`。

**試験：**
- [ ] `brush.rs`：`a_tip_image_maps_its_long_side_to_the_diameter`、`a_tip_outside_the_image_is_transparent`。
- [ ] `brush.rs`：`tip_with_roundness_one_and_angle_zero_matches_the_unrotated_sample`。
- [ ] `browser-tests.js`：`a stamped tip draws its shape`。非対称の画像（L 字）を2つの角度で描き、画素が回転する。`cpuReference` との差が ±2 以内。
- [ ] `browser-tests.js`：`an invalid tip image is refused before the stroke starts`（辺 8、辺 2048、`pixels` の長さ違い）。文書は変わらない。
- [ ] `browser-tests.js`：`GPU loss during a tipped stroke ends with GPU_LOST`（既存の `gpu-stroke` の方式）。文書はストローク前のまま、復元後に同じ先端で描ける。
- [ ] `browser-tests.js`：`a tipped stroke is stored as plain tiles`。先端つきのストロークを描き、保存して開き直したあとの `readTile` が保存前と全バイト一致し、保存した文書の `assets` に `brush_tip` / `pattern` の種類が入っていない（案 A の確認）。
- [ ] `browser-tests.js`：`a tip that exceeds the GPU budget is refused at the first stamp`。小さい `memoryBudgetMB` で最大の先端を使うと `INVALID_ARGUMENT` で拒否され、文書はストローク前のまま、そのストロークは `cancel` できる。

**完了条件：** 上の試験が通り、先端なしのストロークの画素が変わらない。

**検証：** BR-1 と同じ。

### 依存

- 先に要る項目：BR-1（楕円の変形）。
- 後に続く項目：BR-3（バインドグループとテクスチャの確保を共有する）。案 B を選ぶ場合は 09（保存）。

## BR-3 テクスチャブラシ

### 現状

- `AssetKind::Pattern`（`crates/alto-doc/src/node.rs`）は単位バリアントで、使われていない。
- 描画の経路は BR-2 の「現状」と同じ。`brush.wgsl` は、`v = 1 - (1 - v)(1 - shape × d.opacity)` で、ダブごとのカバレッジを累積する。

### 11-04 からの変化

- 変化なし。11-04 版はパターンを `Pattern { width, height }` のアセットにしたが、本版は BR-2 と同じく既定でアセットにしない。

### 設計

カバレッジに紙目のパターンを掛ける。パターンは**レイヤー座標で固定**する（ストロークの向きや筆の動きと無関係）。

- `BrushImage`（BR-2 と同じ型）。`engine.assets.importBrushTexture(file: Blob): Promise<BrushImage>`（新規。取り込みは `importBrushTip` と同じ変換）。辺は 16〜1024。（新規）
- `BrushOptions.tip.texture?: { image: BrushImage; scale?: number; depth?: number }`。`scale` は 0.05〜16（既定 1。texel 座標 = レイヤー画素 × scale）、`depth` は 0〜1（既定 1）。範囲外は `INVALID_ARGUMENT`。
- 式：ダブごとに `shape' = shape × (1 − depth × (1 − pattern(p × scale)))`。累積は `v = 1 - (1 - v)(1 - shape' × d.opacity)`。パターンは画素ごとに固定なので、重なったダブで同じ画素が何度掛かっても、累積は `v` の飽和で収まる。
- `pattern` は texel 座標を `floor` の剰余で巻き戻し（負の座標を含む）、手書きの双線形（4 点の `textureLoad`、巻き戻しあり）で求める。ハードウェアのサンプラーを使わない。
- `GpuStroke.texture_pattern: Option<wgpu::Texture>`（新規。Rgba8Unorm、R を値）、binding 7、`config.info.w` のビット1、`config.params.z = scale`、`config.params.w = depth`。確保と予算の検査、解放は先端と同じ（`stamp` の最初の呼び出し。`begin_stroke` は確保しない）。先端と合わせて `new_bytes` に足す。
- 空のストローク（ダブが 0 個）は何も描かない（既存の `stamp` の早期 return）。
- `cpu_paint` に `texture: Option<(&BrushImage, f32, f32)>` を足す。

**上限：** 辺 16〜1024、`scale` 0.05〜16、`depth` 0〜1。先端画像と合わせても GPU テクスチャは最大 2 × 4 MiB。

### 形式と通信

- `format_version`：案 A は触れない（BR-2 の決定が要る点と同じ）。
- render v3：触れない。
- `PROTOCOL`：`beginBrush` の要求が増えるので上げる（10。BR-1 と同じ段階2の回）。
- シェーダー：`brush.wgsl` の既存モードの変更。

### 作業手順

**Files:** BR-2 と同じファイルに、`importBrushTexture`（`engine.ts`）を足す。

**試験：**
- [ ] `brush.rs`：`depth_zero_leaves_the_coverage_unchanged`、`the_pattern_is_fixed_in_layer_coordinates`（同じ画素を別のダブ位置から描いても、パターンの値が同じ）。
- [ ] `brush.rs`：`the_pattern_wraps_at_negative_coordinates`。
- [ ] `browser-tests.js`：`a textured stroke shows the paper grain`。`depth = 1` で、画素のカバレッジが `cpuReference` と ±2 以内。同じレイヤーで2本のストロークを重ねても、パターンの位置がずれない。
- [ ] `browser-tests.js`：`GPU loss during a textured stroke ends with GPU_LOST`。
- [ ] `browser-tests.js`：`an out-of-range texture scale or depth is refused`。

**完了条件：** 上の試験が通り、テクスチャなしのストロークの画素が変わらない。

**検証：** BR-1 と同じ。

### 依存

- 先に要る項目：BR-2。
- 後に続く項目：なし。

## BR-4 傾きの反映

### 現状

- `crates/alto-raster/src/brush.rs`：`StrokeSample { pos, pressure, tilt: [f32; 2], t_ms: f64 }`。`tilt` は受け取るが `StrokeSampler::input` が読まない。`input` は `pos` と `pressure` の有限性だけを検査し、`tilt` は検査しない。
- `crates/alto-wasm/src/lib.rs` の `brush_input` が、6 個ずつ `[x, y, pressure, tiltX, tiltY, t_ms]` の `f32` 列を読む。
- `packages/alto-sdk/src/tools.ts`：`InputSample { x, y, pressure?, tiltX?, tiltY?, time? }`。`PointerEvent` からは `tiltX` と `tiltY` を送る。マウスは `pressure` が 1、`tilt` が 0。

### 11-04 からの変化

- 変化なし。

### 設計

`tilt` は度（−90〜90）。`angle` と、BR-5 のカーブへの入力 `"tilt"` に使う。

- `BrushDynamics`（新規）に `angle_from_tilt: bool`（既定 false）。真のとき、`tilt` の大きさ `hypot(tx, ty)` が 0 でなければ `tilt_angle = atan2(ty, tx)`、0 のとき（マウス、または直立）は**直前のダブの `tilt_angle` を保つ**（ストロークの最初は 0）。ダブの `angle = config.angle + tilt_angle`（ラジアン）。`atan2(0, 0)` で向きが跳ばない。
- カーブへの入力値 `"tilt"` は `min(hypot(tx, ty) / 90, 1)`。
- `tilt` が非有限（NaN、無限）のサンプルは、`tilt` だけ 0 として扱う（サンプルは捨てない）。
- 傾きの向きは入力デバイスの向きで、レイヤーの回転では回さない。回転したレイヤーでは、傾きから決めた向きとレイヤー上の向きがずれる。既知の制限として SDK のコメントに書く。
- マウス入力（`tilt` が常に 0）では、`angle_from_tilt` が真でも `angle = config.angle` のまま。（新規）

### 形式と通信

- `format_version`：触れない。
- render v3：触れない（ダブ生成だけ）。
- `PROTOCOL`：`beginBrush` の `config` に `dynamics` が増えるので9 に含める（段階1）。
- シェーダー：変更なし（`angle` は BR-1 の `extra` で渡る）。

### 作業手順

**Files:** Modify `crates/alto-raster/src/brush.rs`、`crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/tools.ts`、`protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`。

**試験（`brush.rs`）：**
- [ ] `angle_from_tilt_sets_the_dab_angle`。
- [ ] `no_tilt_keeps_the_previous_angle`（`atan2(0, 0)` で角度が跳ばない。最初のサンプルでは `config.angle`）。
- [ ] `non_finite_tilt_is_treated_as_zero`。
- [ ] `mouse_input_with_angle_from_tilt_matches_a_fixed_angle`。

**試験（`browser-tests.js`）：**
- [ ] `tilt rotates an elliptical dab but not a round one`。`roundness 0.4` で `tiltX` 0 と 45 の画素が変わり、`roundness 1` では全バイト一致する。

**完了条件：** 上の試験が通り、`angle_from_tilt` なしのストロークの画素が変わらない。

**検証：** BR-1 と同じ。

### 依存

- 先に要る項目：BR-1（`Dab.angle` が `extra` の cos / sin としてシェーダーに届く。丸いダブでは見た目に効かない）、BR-5（`BrushDynamics` の型を共有する。同じ PR で入れてよい）。（新規）
- 後に続く項目：なし。

## BR-5 筆圧カーブ、速度連動

### 現状

- `StrokeSampler::dab`（`brush.rs`）：半径 = `size × 0.5 × (pressure_size ? max(pressure, 0.05) : 1)`、`opacity = flow × (pressure_opacity ? pressure : 1)`。間隔 `until` も `size × spacing × (pressure_size ? max(pressure, 0.05) : 1)` を `max(0.25)`。サンプル間は Catmull-Rom で 0.5 px ごとに補間し、筆圧は線形補間。
- `BrushConfig.opacity` は**ストローク全体の上限**（GPU の `min(v, config.params.y)`）。ダブごとの不透明度は `Dab.opacity` があり、`brush.wgsl` が `shape × d.center_radius.w` としてダブごとに掛ける。
- `StrokeSample.t_ms` は受け取るが未使用。`brush_input` が `f32` で渡す（`performance.now()` を `f32` に落とす）。

### 11-04 からの変化

- 変化なし。
- 11-04 版の `dynamics.size` / `opacity` / `flow` の3本のカーブを残す。`Dab.opacity` が既にダブごとに効くので、`opacity` と `flow` のカーブはどちらも実装できる（本版の初稿は誤って `opacity` を除いたが、`Dab.opacity` を見落としていた）。

### 設計

```ts
type BrushCurve = { input: "pressure" | "speed" | "tilt"; points: [number, number][] };
interface BrushDynamics {
  size?: BrushCurve | null;   // 既定：圧力、[[0,0.05],[0.05,0.05],[1,1]]。null で一定（係数1）
  opacity?: BrushCurve | null; // 既定：null（一定）。ダブごとの不透明度（旧 pressureOpacity）
  flow?: BrushCurve | null;   // 既定：null（一定）
  angleFromTilt?: boolean;    // BR-4
}
// BrushOptions.dynamics?: BrushDynamics
```

Rust（新規）：`CurveInput { Pressure, Speed, Tilt }`、`BrushCurve { input: CurveInput, points: Vec<[f32; 2]> }`、`BrushDynamics { size: BrushCurve, opacity: BrushCurve, flow: BrushCurve, angle_from_tilt: bool }`、`BrushConfig.dynamics: BrushDynamics`（`serde(default)`）。`pressure_size` / `pressure_opacity` は削除する。`BrushDynamics::default()` の `size` は `[[0,0.05],[0.05,0.05],[1,1]]`（圧力）で、**今の `max(pressure, 0.05)` と同じ値を返す**。`opacity` と `flow` は一定 `[[0,1],[1,1]]`。SDK は `size` が undefined なら既定のカーブ、`null` なら `[[0,1],[1,1]]` を送る。`opacity` と `flow` は undefined / `null` なら `[[0,1],[1,1]]`。`pressureOpacity: true` に当たるのは `opacity: { input: "pressure", points: [[0,0],[1,1]] }`。検証は `BrushDynamics::validate()` と `BrushConfig::normalized()`（どちらも新規、`brush.rs`）に置き、`begin_brush` が呼ぶ。

**カーブ。** 2〜8 点の区分線形。`x` は狭義単調増加、`points[0].x = 0`、`points.last().x = 1`、`y` は 0〜1、すべて有限。違えば `INVALID_ARGUMENT: brush curve`（`begin_brush` が原子的に拒否し、ストロークは始まらない）。入力が `[0, 1]` の外なら端へ丸める。

**半径と間隔。** `factor = max(curve(input), 0.05)`、半径 = `size × 0.5 × factor`、`until = max(size × spacing × factor, 0.25)`。ダブの `opacity` = `config.flow × flow カーブ(入力) × opacity カーブ(入力)`（`Dab.opacity`。`flow` と `opacity` は同じ乗数に入る。API を11-04 版のまま保つための分け方で、積は交換可能）。`config.opacity`（ストロークの上限）は一定のまま。

**速度。** サンプル間 `dt = t_ms - last.t_ms`、`dx = 距離`（レイヤー画素）。`dt > 0` のとき `v = dx / dt`（px/ms）、指数移動平均 `speed = 0.3 v + 0.7 speed`（最初は `v`）。`dt <= 0`（同じ時刻、逆順、非有限）のときは速度を更新せず直前の値を使う（ゼロ除算なし）。入力値 `min(speed / 4, 1)`。補間中のダブは、区間の始めの値から終わりの値へ線形補間する（圧力と同じ）。

**決定性。** ダブ生成は、サンプル列だけで決まる純粋関数。時刻・乱数・ハードウェアに依らない。`tilt` と `speed` の入力値も、サンプル列だけで決まる。

**cpu_paint：** 変更なし（ダブの `radius` と `opacity` は生成済み）。

### 形式と通信

- `format_version`：触れない。
- render v3：触れない（ダブ生成だけ。既定のカーブは今の出力と同じ）。
- `PROTOCOL`：`BrushConfig` の `pressure_size` / `pressure_opacity` が消え `dynamics` が増えるので9 に含める（段階1）。README と RELEASING を同じ PR で更新する。
- 公開 API：`BrushOptions.pressureSize` / `pressureOpacity` を削除する破壊的変更（決定が要る点）。`packages/alto-sdk/api/alto-engine.api.md` を更新する。
- シェーダー：変更なし。

### 作業手順

**Files:**
- Modify `crates/alto-raster/src/brush.rs`、`crates/alto-wasm/src/lib.rs`（`begin_brush` のカーブ検証、`cpu_brush_reference` の呼び出し）、`packages/alto-sdk/src/tools.ts`、`protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`。
- Modify `tests/e2e/browser-tests.js`（`cpuReference` の `config` を `pressure_size` から `dynamics` に直す。リポジトリ内の他の `pressureSize` の呼び出しも直す）。Playground の呼び出し（`pressureSize` を使う箇所があれば）。

**試験：**
- [ ] `brush.rs`：`curve_interpolates_and_clamps`、`curve_rejects_non_increasing_points`（点数 1・9、`x` が増えない、両端が 0・1 でない、`y` が範囲外）。
- [ ] `brush.rs`：`default_size_curve_equals_the_old_pressure_rule`。圧力 0、0.03、0.05、0.5、1 で、既定の半径が旧式 `size × 0.5 × max(p, 0.05)` と 1e-6 以内で等しい。
- [ ] `brush.rs`：`speed_curve_uses_smoothed_speed`、`repeated_timestamp_does_not_divide_by_zero`、`reversed_timestamp_keeps_the_previous_speed`。
- [ ] `brush.rs`：`the_same_samples_make_the_same_dabs`。
- [ ] `browser-tests.js`：`dynamics opacity by pressure matches the CPU reference`（`pressureOpacity` の代わり）。
- [ ] `browser-tests.js`：`an invalid curve is refused before the stroke starts`。

**完了条件：** 上の試験が通り、既定の設定のストロークが変更前と ±0 で同じ（既存の `GPU brush / pressure / ...` が通る）。

**検証：** BR-1 と同じ。

### 依存

- 先に要る項目：BR-4 と `BrushDynamics` を共有する（同じ PR でよい）。（新規）
- 後に続く項目：なし。

## BR-6 Undo の深さの設定

### 現状

- `crates/alto-doc/src/editor.rs`：`Editor { doc, undo, redo, max_undo: usize, seq, log, digest }`。`max_undo` は非公開で、`Editor::new` が 100 を入れる。`transact` は `undo.len() > max_undo` で先頭を捨て、`restore_history` も同じ上限で切る。`drop_oldest_undo`、`drop_oldest_undos(count)`、`undo_len`、`redo_len` がある。
- `crates/alto-engine/src/lib.rs`：`MAX_HISTORY_ONLY_OBJECTS = 2048`（履歴だけが保つオブジェクトの上限）と `undos_over_budget`。`Engine::maintain` が文書ごとにこれを超える古い Undo を捨てる。`add_document` が `Editor::new` を呼ぶ（`self.documents.insert(handle, Editor::new(doc))`）。
- `Editor` を作って `documents` に入れる場所は、ほかに `crates/alto-engine/src/versions/branches.rs`（`alto_doc::Editor::new(doc)`）と `crates/alto-engine/src/versions/journal.rs`（`rollback_version_state` の `Editor::new(state.doc)`、ほかに他の `Engine` から取り出した editor を入れる箇所）。
- `crates/alto-wasm/src/lib.rs`：`Runtime` が `Engine::new()` を作り直す箇所がある（`self.engine = engine`、`open_recovery_package` など）。
- `packages/alto-sdk/src/engine.ts`：`EngineOptions { container, storage?, gpu?, assets?, maxPixelRatio? }`。`init` の要求（`protocol`、`sdk`、`canvas`、`budget`、`namespace`、`journal`）。`packages/alto-sdk/src/worker.ts` の `case "init"` が `Runtime.create(canvas, budget)` を呼ぶ。

### 11-04 からの変化

- `Editor` と `max_undo` は `crates/alto-doc/src/lib.rs` から `editor.rs` へ移った（`126f2b5`）。値と挙動は変化なし。
- 11-04 版が「既存の `undos_over_budget` の計算に従い、増やさない」と書いた総量の上限は、堅牢化後も同じ（`MAX_HISTORY_ONLY_OBJECTS = 2048`）。

### 設計

`createEngine({ undo: { steps?: number } })`。`steps` は整数で 1〜1000、既定 100。範囲外や非整数は `createEngine` が `INVALID_ARGUMENT` で拒否する（Worker を起動する前）。

**Rust。**
- `Editor::set_max_undo(&mut self, steps: usize)`（新規）：`max_undo` を変え、超える古い Undo を即座に捨てる（`drop_oldest_undos`）。新しいものが残る。`Editor::max_undo(&self) -> usize`（新規）。
- `Engine` に `max_undo: usize`（既定 100）を持たせ、`Engine::set_max_undo(&mut self, steps: usize)`（新規）が、保持する全 editor と今後作る editor に適用する。`documents.insert` の4か所を、上限を適用する小さな関数 `Engine::install_editor(&mut self, handle, editor)`（新規）にまとめる（`add_document`、`branches.rs`、`journal.rs` の2か所）。範囲外は `EngineError` の `INVALID_ARGUMENT`。
- `Runtime`（`crates/alto-wasm/src/lib.rs`）に `setMaxUndo(steps: u32)`（js_name、新規）。`Engine::new()` で作り直す箇所では、直前の上限を引き継ぐ。
- 上限を上げても、`MAX_HISTORY_ONLY_OBJECTS` で `maintain` が古い Undo を捨てる。ただしこれは**ハッシュで数えたオブジェクト数**（ドキュメントが参照しないもの）の上限で、`Vec<Transaction>` 自体のバイト数は数えない。1000 ステップでは、Op の大きさに比例して線形に増える。このアイテムはバイトの上限を足さない。必要なら別途決める（決定が要る点）。

**SDK / Worker。** `EngineOptions.undo?: { steps?: number }`。`init` の要求に `undoSteps: number` を足し、Worker がプロトコルの照合後に `runtime.setMaxUndo` を呼ぶ。再起動でも同じ値を送る。復旧では保存時の上限（メタデータがない場合は 100）でジャーナルを再生してから、新しい上限を適用する。上限が変わった場合、旧世代に戻った場合、またはジャーナル再生が不一致で止まった場合は、次の編集を追記する前にチェックポイントを作る。保存ファイルの欠損や破損では旧世代を試す。一時的な読み取りエラーでは復旧を中断し、両世代を保存したまま再試行できるようにする。

**エラー：** 範囲外は `INVALID_ARGUMENT`（`details.reason` は `"undo-steps"`）。

### 形式と通信

- `format_version`：触れない（Undo 上限は OPFS の復旧チェックポイントに保存する）。
- render v3：触れない。
- `PROTOCOL`：`init` の要求を追加し、段階1のプロトコルを 9 に更新する。README と RELEASING を同じ PR で更新する。
- シェーダー：変更なし。

### 作業手順

**Files:**
- Modify `crates/alto-doc/src/editor.rs`（`set_max_undo`、`max_undo`）、`crates/alto-engine/src/lib.rs`（`Engine.max_undo`、`set_max_undo`、`install_editor`）、`crates/alto-engine/src/versions/branches.rs`、`crates/alto-engine/src/versions/journal.rs`、`crates/alto-wasm/src/lib.rs`。
- Modify `packages/alto-sdk/src/engine.ts`、`protocol.ts`、`worker.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`tests/e2e/browser-tests.js`。

**試験：**
- `editor.rs` の `configurable_undo_trims_oldest_and_applies_to_future_edits` で Undo と Redo 後の上限を確認する。
- `undo_depth.rs` で既存・新規文書、範囲外の値、ロールバックと復旧・版の切り替え後の履歴件数を確認する。
- `browser-tests.js` で範囲外の値、上限到達後の Undo、Worker 再起動、旧世代からの復旧、保存障害と再生の不一致からの再復旧を確認する。

**完了条件：** 上の試験が通り、`undo` を指定しないときの挙動が変わらない（100）。

**検証：** BR-1 と同じ。

### 依存

- 先に要る項目：なし（独立。段階1）。
- 後に続く項目：なし。履歴の打ち切り（08）が `maintain` の上限を扱うときは、`MAX_HISTORY_ONLY_OBJECTS` の定数をここと合わせて見る。

## 上限

| 項目 | 値 | 場所 |
|---|---|---|
| ブラシの大きさ | 0 超 2048 以下 | `begin_brush`（変更なし） |
| 先端画像・テクスチャの辺 | 16〜1024（BR-2、BR-3） | `begin_brush` と `importBrushTip` / `importBrushTexture`（新規） |
| カーブの点 | 2〜8 | `begin_brush` |
| `roundness` | 0.01〜1 に丸める | `begin_brush` |
| `scale` / `depth`（テクスチャ） | 0.05〜16 / 0〜1 | `begin_brush` |
| Undo の深さ | 1〜1000、既定 100 | `createEngine` |
| ダブ数 | 明示の上限なし。ストロークのメモリ見積もりが GPU 予算または CPU の読み戻し予約（512 MiB）を超えると `RenderError::Budget`（コード `INVALID_ARGUMENT`）で拒否し、ストロークは続けられる | `Renderer::stamp`（変更なし） |

エラーコードは既存の `INVALID_ARGUMENT`、`GPU_LOST`、`CONFLICT` を使い、新しいコードは足さない。

## Review Focus

- `tilt` を持たないマウス入力で、値が 0 でも動作が変わらないこと。
- 既存の丸いブラシの結果（`cpuReference` との ±2 の比較）が全く変わらないこと。`roundness = 1` と画像・テクスチャなしのダブは、ビット単位で同じ出力であること。
- 細い楕円（`roundness` 0.01）で距離の計算がゼロ除算にならないこと。
- 角度とパターンがレイヤー座標で測られ、回転した表示やレイヤー変形の下で一貫すること。
- 不正な設定（カーブ、画像、`scale`、`depth`）を、ストロークを始める前に拒否し、文書を変えないこと。
- GPU 喪失時に画像先端・テクスチャのストロークが `GPU_LOST` で終わり、次のストロークが同じ画素列から作り直せること。
- `PROTOCOL` を上げる PR で README と RELEASING を更新したこと。
- 07（ベクター）と 02（効果）が `renderer.rs` の同じ束ね方を変える場合は、`brush.wgsl` の binding の番号を衝突させないこと。
