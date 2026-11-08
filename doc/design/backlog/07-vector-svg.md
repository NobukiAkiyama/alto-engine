# ベクターと SVG

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** ベクターの塗りと線の表現を増やし、SVG 読み込みの対象を広げる。

**Architecture:** `alto-vector` の `StyledPath` が正本。v3 は `raster_q8` の整数カバレッジで描き、色を Q14 で載せる。

## 現状

- `StyledPath { data, fill: Option<Color>, stroke: Option<Stroke>, fill_rule }`、`Stroke { color, width, cap, join, miter_limit }`。
- `crates/alto-engine/src/svg.rs`：`svg`、`g`、`path`、`rect`、`circle`、`ellipse`、`line`、`polyline`、`polygon` のみ。未対応は位置付きエラーで全体を拒否する。
- 円弧は Cubic へ正規化する。

## VEC-1 塗りのグラデーション

**データ。** `fill` と `stroke.color` の型を `Paint` へ広げる。

```rust
#[serde(untagged)]
pub enum Paint { Solid(Color), Gradient(Gradient) }
pub struct Gradient { kind: GradientKind, from: [f64; 2], to: [f64; 2], stops: Vec<GradientStop> }
```

`Gradient` の形は [02-effects.md](02-effects.md) の FX-9 と共通。座標はパスと同じノード座標。

**描画。** カバレッジ（既存）× 勾配の色。`vector-q14.wgsl` の着色で `gradient_q14` を使う。インスタンス単位の色をストレージバッファの勾配番号に広げる。`raster_q8::vector_pixels_q14` も同じ計算へ揃える（IMG-2 の確定に使う）。単色の経路とハッシュは不変。

## VEC-2 破線

**設計。** パス段階で線分に分割してから既存の線の展開へ渡す。描画は変わらない。

- `Stroke.dash: Option<Dash { pattern: Vec<f32>, offset: f32 }>`。pattern は 1〜16 要素、合計 > 0、全要素 ≥ 0。奇数個は2回繰り返す（SVG と同じ）。
- 長さの計算は kurbo の弧長で、許容誤差は定数（0.01 ノードpx）に固定し、文書の結果を決定的にする。
- 分割後の命令数が `4096`（パス当たり）を超えたら拒否する。

## VEC-3 SVG の CSS、画像、文字、外部参照

**設計。** 段階で分ける。全項目で「非対応は部分 import せず拒否」の原則を守る。

1. **`style` 属性と `<style>` の単純セレクタ。** 型、`.class`、`#id` のみ。結合子と擬似クラスは拒否。プロパティは既存の属性と同じ集合。
2. **`<linearGradient>`、`<radialGradient>`。** VEC-1 に写す。`gradientUnits` の両方。`spreadMethod` は pad のみ。
3. **`stroke-dasharray`。** VEC-2 に写す。
4. **`<image>`。** `data:` URI の PNG / JPEG のみ。ラスターレイヤーへ。64M 画素の上限を適用する。
5. **`<text>`。** `font-family` を文書の Font アセットから解決する。解決できなければ拒否。テキストレイヤーへ。`textPath` は拒否。
6. **外部参照。** `<use href="#id">` の文書内参照のみ展開し、参照の循環を検出する。深さ 16、展開後のノード数に上限。外部 URL（`http:`、相対ファイル）は**常に拒否**する。読み込みでネットワークへ出ない。

XML の展開攻撃に備え、`roxmltree` の `allow_dtd` は無効のまま。全項目を QA-1 のファザーに含める。

## VEC-4 vello 本体描画

**設計。** `VectorRasterizer` trait と比較ページが既にある。本体の v3 は全バイト一致の整数経路で、vello の浮動小数の結果を正本にできない。そこで「表示の高速経路」に限り、確定（読み戻し、書き出し、ハッシュの対象）は整数経路に残す。

- `createEngine({ vectorPreview: "integer" | "vello" })`。既定は `integer`。
- vello の結果は表示タイルのキャッシュに入れ、`preview` の印を付ける。ビューが止まったら、整数経路で描き直して置き換える（見た目の差はそこで解消する）。
- WASM の増加が大きいので、別の WASM パッケージ（`vector-comparison` と同じ形）に分け、選んだときだけ読み込む。
- 判定：[比較結果](../../verification/VECTOR-COMPARISON.md) と同じ条件で、密集ベクターの描画が整数経路の2倍以上速くない場合は実験的な選択肢のままにする。

## 実装タスク

### Task VEC-2: 破線

**Files:** Modify `crates/alto-vector/src/lib.rs`、`crates/alto-gpu/src/raster_q8.rs`、`packages/alto-sdk/src/types.ts`。

**Interfaces:** Produces `Stroke.dash`、`fn dash_path(path: &PathData, dash: &Dash) -> Result<PathData, VectorError>`。

- [ ] 試験 `dash_splits_a_line_at_the_pattern`、`dash_offset_shifts_the_phase`、`odd_pattern_repeats_twice`。
- [ ] 試験 `all_zero_pattern_is_refused`、`dash_past_the_command_limit_is_refused`。
- [ ] 試験 `no_dash_serializes_the_same_bytes`（既存のスナップショットのハッシュが不変）。
- [ ] 試験 `closed_path_dashes_over_the_close_segment`。
- [ ] 実装、PR。

### Task VEC-1: 塗りのグラデーション

**Interfaces:** Consumes `gradient_q14`（FX-9）。Produces `Paint`、`Gradient`。

- [ ] 試験 `solid_paint_serializes_as_before`。
- [ ] parity ケース `vector-gradient-linear`、`-radial`、`-stroke`、`-transform`。
- [ ] 試験 `rasterizing_a_gradient_matches_the_display`。
- [ ] 実装、PR。

### Task VEC-3: SVG の拡張

**Files:** Modify `crates/alto-engine/src/svg.rs`。段階ごとに PR。

- [ ] 試験 `style_attribute_overrides_presentation_attribute`、`unsupported_selector_is_refused_with_position`。
- [ ] 試験 `svg_gradient_maps_to_paint`、`object_bounding_box_units_map_to_node_coordinates`。
- [ ] 試験 `use_cycle_is_refused`、`use_expansion_past_the_limit_is_refused`。
- [ ] 試験 `external_image_href_is_refused`、`http_use_href_is_refused`。
- [ ] 試験 `unknown_font_family_is_refused`。

### Task VEC-4: vello の表示経路

**Files:** Modify `crates/alto-gpu/src/renderer/vector.rs`、`crates/alto-wasm/src/lib.rs`、`scripts/build-vector-comparison.sh`、`packages/alto-sdk/src/engine.ts`、`worker.ts`。

- [ ] 現在の整数経路と vello の時間を測り、判定を記録。
- [ ] 試験 `preview_tiles_are_never_read_back`（書き出し、確定、ハッシュが整数経路の結果）。
- [ ] ブラウザ試験 `a settled view replaces preview tiles`、`the default build has no vello`。
- [ ] GPU 喪失と、パッケージの読み込み失敗時の整数経路への復帰を試験。
- [ ] 実装、PR。

## Review Focus

- 破線の分割結果が、線端（cap）の扱いで既存の閉じたパスと矛盾しないこと。
- 拡大縮小で勾配がパスと一緒に動くこと（レイヤー変形、マスクの独立変形）。
- `canvas.resize` で勾配の座標と破線の長さも更新すること。
- 差分の表示（`DiffItem`）で勾配の変更が内容の変更として見えること。
- SVG の `<use>` の展開で、ID の衝突や NodeId の生成が決定的であること。
