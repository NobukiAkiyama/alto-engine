# ベクターと SVG

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** ベクターの塗りと線の表現を増やし、SVG 読み込みの対象を広げる。

**Architecture:** `alto-vector` の `StyledPath` が正本で、座標はノード座標（f64）。render v3 は唯一の描画版で（`DocumentMeta::validate` が v3 以外を拒否する）、ベクターの被覆はすべて CPU の整数で求める。パスは `raster_q8::map_path` でレベル画素の Q8 へ写し、塗りは `raster_q8::rasterize_crop_prepared`、線は `stroke_q8::outline`（ローカル座標で線の輪郭の多角形を作る）を通して同じ整数ラスタライザへ渡す。GPU は被覆の crop を `glyph-q14.wgsl` で色に重ねるだけである。lyon と vello は v3 の経路に入っていない。

## 共通の現状

- 型：`crates/alto-vector/src/lib.rs` の `StyledPath`（`data`、`fill: Option<Color>`、`stroke: Option<Stroke>`、`fill_rule`）、`Stroke`（`color`、`width`、`cap`、`join`、`miter_limit`）、`Color = [f32; 4]`（straight sRGB）。全型が `deny_unknown_fields`。`StyledPath::fingerprint` は serde_json 化したバイト列のハッシュなので、フィールドの直列化が変わると値も変わる。
- 検証：`StyledPath::validate`（色は 0..=1、線幅 0..=4096、miter limit 1..=64）、`PathData::validate`（パス当たり4096命令、座標は ±1048576）、`validate_paths`（レイヤー当たり128パス、16384命令）。これらは `Document` の検証と `Op::SetVector` から呼ばれる（`crates/alto-doc/src/document.rs`）。
- v3 の描画経路（呼び出しを追った結果）：
  1. `RContent::Mesh` → `Context::vector_q14` → `vector_q14_prepared`（`crates/alto-gpu/src/renderer/tile_renderer/paint.rs`、呼び出し元は `graph.rs`）。ベクターマスクとクリップの被覆も同じ `vector_q14`（白の塗り1本）を通る。
  2. 塗り：`raster_q8::map_path` → `raster_q8::Prepared` → `rasterize_crop_prepared`。線：`stroke_q8::outline` → `Prepared::new` → `rasterize_crop_prepared`（非ゼロ規則）。線の輪郭は `PreparedVector.outlines` に詰めて保持する（`PREPARED_BYTES` 32 MiB、`PREPARED_VECTORS` 32、キーは `Arc<Vec<StyledPath>>` の同一性とワールド行列とレベル）。
  3. 色：`raster_q8::color_q14`（f32 のビット列から Q14 へ half-away で丸める）。パスごとに塗り→線の順で `GlyphCrop`（crop の被覆バイトと Q14 の単色）にし、`paint_crops` が `glyph-q14.wgsl` で重ねる。色は crop 全体で一定である。
  4. `vector-q14.wgsl` は `crates/alto-gpu/src/renderer.rs` のシェーダー一覧と `pool_tests.rs` にだけ残り、描画には使われない。式は `glyph-q14.wgsl` と同じ。
- 線の輪郭は `stroke_q8.rs` の中で閉じている：`flatten_stroke`（`raster_q8.rs`、整数の曲線分割、1サブパス 2^18 線分まで）→ 線分ごとに本体の四角形 → 線端と結合の多角形（`round_wedge`、`miter`）→ ワールド行列で Q8 へ。輪郭の命令は 2^20 まで（`MAX_OUTLINE_COMMANDS`）。超えると `RasterError::CurveRange`（全タイルで同じくそのパスの線を飛ばす）。
- CPU 参照：Rust は `raster_q8::vector_pixels_q14`（`stroke_q8::rasterize_stroke` を呼ぶ。IMG-2 の `canvas.rs` の `VectorRasterizer` 注入と `crates/alto-wasm/src/image_versions.rs` から使われる）と `cpu_composite/v3_tests.rs`。JS 参照 `tests/e2e/cpu-composite.js` の `renderVectorV3` は**塗りだけ**で、線は持たない。ベクターのブラウザー parity は `tests/e2e/parity-tests.js` の `vector-v3-*` で、すべて塗りである。
- lyon は legacy の経路：`alto_vector::LyonRasterizer`（`renderer/vector.rs`）。render v1・v2 は非対応になったので、v3 の文書はここを通らない。vello は `vello-probe` feature（`crates/alto-wasm/src/vector_probe.rs`）の比較用だけ。
- SVG：`crates/alto-engine/src/svg.rs` の `Engine::prepare_svg_import`。要素は `svg`、`g`、`path`、`rect`、`circle`、`ellipse`、`line`、`polyline`、`polygon` だけ。属性は `id`、`fill`、`stroke`、`fill-opacity`、`stroke-opacity`、`stroke-width`、`stroke-linecap`、`stroke-linejoin`、`stroke-miterlimit`、`fill-rule`、`opacity`、`transform` と形ごとの幾何属性。色は `#rgb`、`#rrggbb`、`black`、`white`、`none`。未対応は位置付き（`UNSUPPORTED`、バイト・行・列）で全体を拒否する。上限と、超えたときのコード：4 MiB は `RESOURCE_LIMIT`。`roxmltree` の `nodes_limit` 30000（XML の解析エラー）、事前検査の要素 10000、深さ 64、合計命令 65536 は `UNSUPPORTED`（`error()` を通る）。`Builder::push` のノード数 10000 は `RESOURCE_LIMIT`。DTD は無効（`allow_dtd: false`）。円弧は kurbo ではなく `svgtypes` の `SimplifyingPathParser` で Cubic にする（10倍に拡大して 0.1 の許容を 0.01 にしている）。
- キャンバス編集：`canvas.resize`・`rotate`・`flip` はルートノードの変形、リンクしていないマスクの変形、選択の画素を書き換え、パス、勾配、破線は触らない（`crates/alto-engine/src/canvas.rs` の `prepare_canvas_edit`）。文書画素の単位を持つ値（ぼかし半径）だけ `scale_effects` で更新する。
- 差分：`diff`（`crates/alto-doc/src/version.rs`）はベクターの変更を `ContentDetail::Vector { paths, commands }`（本数と命令数）で返す。SDK は `packages/alto-sdk/src/types.ts` の `ContentDetail`。
- SDK：`packages/alto-sdk/src/types.ts` の `StyledPath`、`VectorStroke`。編集は `doc.vectors.set`（`document.ts`）と `tx.addVector`・`tx.setVector`（`transaction.ts`）。SVG は `doc.importSvg`。

## VEC-1 塗りのグラデーション

### 現状

- 塗りと線の色は `Option<Color>` と `Stroke.color: Color` の単色だけ。勾配の型は `alto-vector` に無い。
- ラスター確定の勾配は別物：`GradientOptions`（`crates/alto-engine/src/image/mod.rs`、`start`、`end`、`stops`、`kind` が `linear`／`radial`）。停止点は 2〜16 個、offset は 0..=1 で重複不可、色は f64。ベクターの勾配には流用しない（直列化の形が違う）。
- 着色の関数は「共通の現状」3 のとおり crop 単位の単色で、画素ごとに色が変わる経路は無い。
- `raster_q8::stroke_visible` は線の `color[3]` から可視を決める。`renderer/uniform.rs` は可視の線があるシーンを一様サブツリーの近道から外すが、塗りの色には触れない（塗りは「画素が被覆バイトだけで決まる」前提）。

### 11-04 からの変化

- 11-04 は着色を `vector-q14.wgsl` と書いていた。979f1d7 で、パスごとの dispatch をやめて crop をまとめ `glyph-q14.wgsl` で重ねる形になった。`vector-q14.wgsl` は描画に使われない。
- 同じ変更で、座標変換（`map_path`）と線の輪郭がノード単位で保持されるようになった（`PreparedVector`）。勾配は被覆に依存しないので、この保持の対象外にできる。
- 11-04 は FX-9 を「共通シェーダー関数を WGSL の共通ファイルから使う」と書いた。FX-9 は 02-effects.md の項目で、共通部分（`Gradient`、`gradient-q14.wgsl`）は VEC-1 が先に作る（00-ROADMAP の重なりの表）。

### 設計

**データ。** 勾配の型は `alto-core`（`crates/alto-core/src/gradient.rs`（新規））に置く。`alto-vector` と `alto-doc`（FX-9）の両方が依存するのは `alto-core` だけだからである。

```rust
/// 値の形は VEC-1 と FX-9 で共通（新規）。
#[serde(deny_unknown_fields)]
pub struct Gradient {
    pub kind: GradientKind,                  // "linear" | "radial"
    pub from: [f64; 2],                      // 線形は始点、放射は中心
    pub to: [f64; 2],                        // 線形は終点、放射は円周上の点（半径 = |to - from|）
    pub stops: Vec<GradientStop>,            // { offset: f64, color: [f32; 4] }（straight、0..=1）
    #[serde(default, skip_serializing_if = "Affine2::is_identity")] // is_identity は新規
    pub transform: Affine2,                  // 勾配空間 -> ノード座標。既定は単位
}
```

- `StyledPath.fill: Option<Paint>`、`Stroke.color: Paint`（フィールド名は `color` のまま）。

```rust
#[serde(untagged)]
pub enum Paint { Solid(Color), Gradient(Gradient) }
```

単色は従来どおり4要素の配列で直列化されるので、勾配を使わない文書の `fingerprint` とハッシュは変わらない。untagged の誤りは不明瞭な文言になるので、`Deserialize` を手で書き、配列なら `Solid`、オブジェクトなら `Gradient` として、失敗は `INVALID_ARGUMENT: paint` にする。
- `transform` を持つのは、SVG の gradientTransform と objectBoundingBox（縦横比が違う図形の放射は楕円）を、端点だけでは写せないからである。FX-9 は単位行列のままでよい。
- 検証（`Gradient::validate`（新規））：`from` と `to` は有限で ±1048576 以内、距離 ≥ 1/256（二乗距離 ≥ 2^-16）。停止点は 2〜16 個、`offset` は 0..=1 で**非減少**（同じ offset は硬い境目を作り、その点では後ろの色を使う）、色は `color()` と同じ検査。`transform` は有限で `FixedAffine::from_affine` が成功し、逆行列がある。`StyledPath::validate` が塗りと線の両方に適用する。
- FX-9 との差：既存のラスター勾配は offset の重複を拒否する。FX-9（`02-effects.md`）は同じ `Gradient` を案 A で使い、試験名は stops_must_not_decrease である。FX-9 の案が変われば、ここの「非減少」と合わせる。

**評価（整数のみ）。** ホストが画素から勾配の位置 t（Q16）への写像を整数係数で渡し、シェーダーは係数と画素座標だけで t を求める。

1. ホスト（`raster_q8::gradient_plan`（新規））：ワールド行列（Q32、`FixedAffine`）とレベルから「窓の画素中心 → ノード座標」の逆写像を作り、`transform` の逆と `from`、`to` を合成する。この計算は f64 の四則だけで行い（sqrt、三角関数を使わない）、係数は既存の `alto_core::q_from_f64_bits(x.to_bits(), 32)` で Q32 の整数にする。f64 の四則は IEEE で決まるので、機種によらず同じビットになる。
2. 線形：`t_q16 = clamp(floor((ax*px + ay*py + a0) / 2^24), 0, 65536)`。`px`、`py` は窓の画素中心の Q8。係数 `ax`、`ay`、`a0` は「t_q16 / Q8 単位」を Q24 にした整数（Q8 の座標と Q16 の t のスケール差を係数に含める）。放射：画素の二次式 `S = cxx*px^2 + cxy*px*py + cyy*py^2 + cx*px + cy*py + c0` を Q32 の `s^2` とし、`t_q16 = isqrt(min(S, 2^32))`。`isqrt` は 16 回の二分法（符号なし 32 ビット）で、ニュートン法は使わない（FX-9 と回数を固定する点だけ合わせる）。
   窓の隅で `S` を厳密に評価する：`S0 = S(窓の原点)` と、窓内の差分 `d`（0 以上 256 未満）の項 `L·d + Q(d)` を、ホストが i128 で厳密に求めて渡す。`S0` は 2^40 で飽和させる。ノード単位の検査（5）で `|L·d + Q(d)| < 2^39` を保証するので、飽和した画素は必ず `S >= 2^32`（端の色）になり、飽和の有無は窓によらない。シェーダーは `S0` と小さな差分項だけを `Wide` で足す。
3. 色：停止点の色を `color_q14` で Q14 の straight に、offset を Q16 にする。`t` が属する区間 `i`（`offset[i] <= t` を満たす最大の `i`。同じ offset が並ぶ場合は後ろ）を探し、先頭より前と末尾以降は端の色（`spreadMethod=pad`）。区間内では `w = div_half_away((t - o_i) << 16, o_{i+1} - o_i)`（分子は 2^32 未満で u32 に収まる）、各チャンネル `c = c_i + div_half_away((c_{i+1} - c_i) * w, 65536)`。補間は straight のまま（SVG と同じ）。
4. 重ね：得た straight の Q14 色を、既存の式 `source_over_q14`（被覆バイト → `k = (cov*16384+127)/255`、`f = k*color.a`、…）の `color` として画素ごとに使う。式は変えない。
5. 範囲：量子化した係数から、窓内の差分の項 `|L·d + Q(d)|` の上界を i128 で求め、2^39 以上になる写像、または係数が i128 に収まらない写像は、`VECTOR_TRANSFORM_RANGE` と同じく**ノード全体が透明**にする（タイルごとに決まらない）。この判定は係数だけで決まり、窓にもタイルにも依存しない。中心が遠い小さい円（中心 2^20、半径 1/64）では `S0` が 2^40 を超えるが、差分の項は小さいので範囲内で、原点のタイルは端の色になる。

**GPU。** crop に勾配番号を持たせた別の dispatch にする。

- 新規ファイル：`crates/alto-gpu/shaders/gradient-q14.wgsl`（新規）は FX-9 の勾配の平面と共有する関数 `gradient_q14(t_q16, stops)`（新規）の本体。戻り値の型は `02-effects.md` の定義（現状 `vec4i`）に合わせ、`glyph-q14.wgsl` の `vec4u` へは呼び出し側で変換する。`crates/alto-gpu/shaders/vector-gradient-q14.wgsl`（新規）は `glyph-q14.wgsl` と同じ構成で、色を画素ごとに `gradient_q14`（新規）から取る。浮動小数は `unorm_byte` だけ。
- `paint_crops` は単色の crop と勾配の crop が混ざるとき、種類が変わる所で `pending` を流して順序を保つ。勾配の記録（係数と停止点16個）は storage buffer に入れ、`need` の予算に加える。パイプラインは `glyph_q14` と同様に `Resources` へ（`vector_gradient_q14`（新規））。`renderer.rs` のシェーダー一覧と `pool_tests.rs` にも登録する。
- `uniform.rs` の近道：勾配の塗りは画素ごとに色が変わり、端から離れた葉の画素が同じバイトになる前提が崩れるので、`edges` は勾配の塗りまたは線を見つけたら `None`（近道を取らない）にする。
- 線の勾配は、塗りと同じ勾配空間（ノード座標）を使う。SVG も線に同じ座標系を使う。

**他の経路。**

- `raster_q8::stroke_visible` は、勾配では「どれかの停止点の Q14 アルファが0でない」で可視にする。
- `vector_pixels_q14`（IMG-2 の確定）は同じ `gradient_plan` と `gradient_q14` の Rust 参照（i128、`raster_q8::gradient_q14`（新規））で色を求める。これが GPU の正解になる。
- JS 参照（`renderVectorV3`）は塗りの勾配だけ、BigInt で独立に実装する。線は JS 参照に無いので、Rust の試験だけで確かめる。
- legacy の lyon（`LyonRasterizer::tessellate`）は勾配を持てない。`Paint::Gradient` を見つけたら `VectorError` を返す（v3 の文書は lyon に来ない）。
- `ContentDetail::Vector` に `style: bool`（`#[serde(default)]`）を足し、本数と命令数が同じで塗りか線の設定だけが変わった場合に `true` とする。勾配の変更でも `ContentChanged` は出ているが、詳細が空に見えるのを防ぐ。SDK の `ContentDetail` と API レポートも更新する。
- `canvas.resize` の更新は不要：勾配の座標はノード座標で、キャンバス編集はルートの変形だけを書き換える。非一様な拡大では、線幅と同じように勾配も一緒に歪む。

**SDK。** `types.ts`：

```ts
export interface VectorGradientStop { offset: number; color: Color }
export interface VectorGradient {
  kind: "linear" | "radial";
  from: [number, number];
  to: [number, number];
  stops: VectorGradientStop[];
  transform?: Affine;
}
export type VectorPaint = Color | VectorGradient;
// VectorStroke.color: VectorPaint, StyledPath.fill: VectorPaint | null
```

プレイグラウンドの `examples/browser-playground/vector-ui.js` は `p.fill` を配列として読む。勾配のときは「勾配」と表示して色の入力を無効にするだけにする（UI の追加は対象外）。

### 形式と通信

- `format_version`：**上げる**（7。00-ROADMAP の番号の表）。`fill` と `stroke.color` に新しい形が入り、古い実装は `deny_unknown_fields` で読めない。VEC-2 の `dash` と同じ番号にまとめる。`DocumentMeta.format_version` は全スナップショットに入るので、文書全体のバイト列は変わる（単色だけの文書も）。変わらないのは `StyledPath` の直列化と `fingerprint` で、試験で固定する。
- `format_version` を上げるとき、パッケージの manifest の `document_format` も同じ値に上げる必要がある。書き出しは 5 を固定で書き（`crates/alto-store/src/lib.rs`）、読み込みは 5 以外を `package format` として拒否する。書き出しの定数と読み込みの受け入れを同じ PR で変え、`crates/alto-doc/src/lib.rs` の `DocumentMeta::validate` と合わせる（値は 7。`FORMAT_VERSION`／`SUPPORTED_FORMATS` と manifest・`gc.rs`・`merge.rs` の経路は回 6 の PR（06）が先に入れるので、この PR は 7 を足す。回 6 より先に入るなら、この PR が基盤を入れ、番号を取り直す）。（新規）
- `PROTOCOL`：上げない。新しい要求は無く、`setVector` のペイロードの型が広がるだけである。
- 描画の版：v3 のまま。既存の画素と `tests/parity/` の既存行は変わらない（共通規則1）。勾配は新しい行を足すだけである。
- シェーダーのモード番号：使わない（効果の段ではなく crop の着色 dispatch）。FX-9 の勾配の平面（モード 42）と `gradient-q14.wgsl` を共有する。

### 作業手順

**Files:**
- Create `crates/alto-core/src/gradient.rs`（新規。FX-9 が先に作った場合は Modify）、`crates/alto-gpu/shaders/gradient-q14.wgsl`（新規。FX-9 と共有）、`crates/alto-gpu/shaders/vector-gradient-q14.wgsl`（新規）。
- Modify `crates/alto-core/src/lib.rs`、`crates/alto-vector/src/lib.rs`、`crates/alto-vector/src/glyph.rs`、`crates/alto-engine/src/vector_tests.rs`、`crates/alto-engine/examples/create-vector-sample.rs`、`crates/alto-gpu/src/cpu_composite/known_value_tests.rs`、`crates/alto-wasm/src/vector_probe.rs`、`crates/alto-gpu/src/raster_q8.rs`、`crates/alto-gpu/src/renderer/tile_renderer/paint.rs`、`crates/alto-gpu/src/renderer/tile_renderer/mod.rs`、`crates/alto-gpu/src/renderer.rs`、`crates/alto-gpu/src/renderer/tile_renderer/pool_tests.rs`、`crates/alto-gpu/src/renderer/uniform.rs`、`crates/alto-gpu/src/cpu_composite/v3_tests.rs`、`crates/alto-doc/src/version.rs`、`crates/alto-doc/src/lib.rs`（`format_version` 検査）、`crates/alto-store/src/lib.rs`（manifest の `document_format` の書き出しと受け入れ。試験 `package_with_new_document_format_roundtrips`、旧番号は拒否）、`crates/alto-engine/src/svg.rs`、`crates/alto-engine/src/text.rs`、`crates/alto-text/src/outline.rs`（`Paint::Solid` への置き換え）、`tests/e2e/cpu-composite.js`、`tests/e2e/parity-tests.js`、`tests/parity/v3-browser.txt`（追記のみ）、`packages/alto-sdk/src/types.ts`、`packages/alto-sdk/api/alto-engine.api.md`、`examples/browser-playground/vector-ui.js`。

**Interfaces:** Produces `Paint`、`Gradient`、`GradientKind`、`GradientStop`、`Gradient::validate`、`raster_q8::gradient_plan`、`raster_q8::gradient_q14`、`vector_gradient_q14` パイプライン。Consumes なし。`gradient_q14` の WGSL は本項目が作り、FX-9 が使う（00-ROADMAP の重なりの表）。

- [ ] 手順1：`Paint` と `Gradient` を入れ、`fill` と `stroke.color` の型を変えて全呼び出し元（上の Modify の一覧）を `Paint::Solid` でビルドが通るようにする。この時点で描画の挙動は変えない。
- [ ] 試験（`alto-vector`）`solid_paint_serializes_as_before`（既存のスナップショットの `fingerprint` が不変）、`gradient_roundtrips_through_json`、`paint_array_and_object_are_told_apart`。
- [ ] 試験 `gradient_rejects_one_stop`、`gradient_rejects_decreasing_offsets`、`gradient_accepts_equal_offsets_as_a_hard_edge`、`gradient_rejects_coincident_endpoints`、`gradient_rejects_singular_transform`、`gradient_rejects_nan_and_oversized_coordinates`（原子的に拒否され、文書は変わらない）。
- [ ] 手順2：`raster_q8::gradient_plan` と `gradient_q14`（i128）を実装。試験 `gradient_q14_two_stops_midpoint`（手計算した既知の値）、`gradient_q14_pads_outside_the_range`、`gradient_q14_hard_edge_takes_the_later_color`、`radial_center_equals_first_stop`、`gradient_plan_extremes_do_not_overflow`（範囲外はノード全体が透明）、`gradient_plan_is_independent_of_the_window`（窓をずらしても同じ画素の色が同じ）。
- [ ] 手順3：WGSL と dispatch。`cpu_composite/v3_tests.rs` に `v3_linear_gradient_fill_matches_the_integer_reference`、`v3_radial_gradient_stroke_matches_the_integer_reference`、`v3_gradient_follows_the_node_transform`（回転・非一様拡大）、`v3_gradient_follows_the_mask_transform`、`v3_solid_and_gradient_paints_keep_their_order`（種類が交互のとき順序が守られる）。GPU の結果が参照とバイト一致すること。Metal、lavapipe、SwiftShader で一致すること。
- [ ] 試験 `uniform_subtree_is_not_taken_for_a_gradient_fill`（`uniform.rs`）。
- [ ] 手順4：JS 参照。parity ケース `vector-gradient-linear`、`vector-gradient-radial`、`vector-gradient-transform`（塗りのみ）を `tests/e2e/parity-tests.js` へ、行を `tests/parity/v3-browser.txt` へ追記（`ALTO_WRITE_V3_BROWSER=1 node scripts/verify-browser.mjs --suite=parity`）。線の勾配は Rust の試験だけで、ブラウザーの parity は作らない。
- [ ] 試験 `rasterizing_a_gradient_matches_the_display`（`vector_pixels_q14` の画素が表示と一致。IMG-2 の確定に使えること）。
- [ ] 試験（`alto-doc`）`diff_reports_a_gradient_change_as_a_style_change`。`canvas.resize` 後も勾配の座標が変わらず、描画がルート変形に従うことの試験 `gradient_survives_canvas_resize`（`canvas.rs`）。
- [ ] SDK：型、API レポート、`pnpm --dir packages/alto-sdk build`。
- [ ] 完了条件：`tests/parity/` の既存行が1行も変わらない（`git diff tests/parity` で追記のみ）。`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional` が成功。opencode レビュー後に PR。

### 依存

- 先に：なし。`Gradient`（`crates/alto-core/src/gradient.rs`（新規））、`gradient-q14.wgsl`（新規）、停止点の格納の持ち主は本項目で、FX-9（02）より先に入れる（00-ROADMAP の重なりの表）。VEC-3 の段階2が VEC-1 に依存する。
- 後に：VEC-3 段階2（SVG の勾配）、IMG-2（勾配のベクターの確定）。FX-9 とは型と WGSL を共有する。

## VEC-2 破線

### 実装前

- `Stroke` に破線は無い。線の輪郭は `crates/alto-gpu/src/stroke_q8.rs` の `outline` が作る：サブパスごとに `subpaths` で分け、`raster_q8::flatten_stroke` で整数の折れ線にし、線分ごとに本体、端に線端（butt、round、square）、内側の頂点に結合（miter、round、bevel）を足す。閉じたサブパス（`Close` を持つ）は最後の線分から最初への結合まで作り、線端を作らない。
- `outline` はローカル座標（ノード座標を `local_bits(world, level)` で Q8〜Q32 の整数にしたもの）で計算し、多角形ごとに `map_point` でレベル画素の Q8 へ写す。幅、miter limit、破線の長さもこのローカルの整数にする。
- 線を作る呼び出し元は3つ：`paint.rs`（`outline`）、`raster_q8::vector_pixels_q14`（`rasterize_stroke`）、legacy の lyon（`LyonRasterizer::tessellate`、`StrokeOptions`。lyon に破線は無い）。`uniform.rs` は可視の線があるシーンを近道から外す。
- JS 参照に線は無い。線の試験は Rust の `cpu_composite/v3_tests.rs`（`v3_curved_stroke_matches_the_integer_reference`）と `stroke_q8.rs` 内の試験（`seam_coverage_is_window_independent`、`curve_precision_tracks_the_zoom_level`）。
- SVG の `stroke-dasharray` は未対応（属性として拒否される）。

### 11-04 からの変化

- 11-04 は「パス段階で線分に分割してから既存の線の展開へ渡す」「長さは kurbo の弧長」「`raster_q8.rs` を変更」と書いた。実際の線の展開は `stroke_q8.rs` にあり、すべて整数である。kurbo の f64 の弧長は v3 の「ホストは整数へ量子化してから渡す」に反するので、破線も**整数の折れ線の上で**分割する形に変える。
- 979f1d7 で線の輪郭がノード単位で保持される（`PreparedVector.outlines`）ようになった。破線は輪郭の一部になるので、保持の対象にそのまま入る。

### 設計

**データ。** `Stroke` に追加する。

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub dash: Option<Dash>,
#[serde(deny_unknown_fields)]
pub struct Dash { pub pattern: Vec<f32>, #[serde(default)] pub offset: f32 }
```

- 長さの単位はノード座標（線幅と同じ）。`dash` が無い文書の直列化は変わらない。
- 検証（`Dash::validate`（新規））：`pattern` は 1〜16 要素（`MAX_DASH_ELEMENTS`（新規））、各要素は有限で 0..=1048576、周期（奇数個は2回繰り返して合計）> 0 かつ ≥ 1/64、`offset` は有限で ±1048576 以内（負でもよい）。全要素が0のパターンは拒否する（SVG の importer は「合計0は `none`」として `None` にする）。
- 破線の数の上限（`StyledPath::validate` で編集時に拒否）：サブパスごとに制御点を結ぶ折れ線の長さ（弧長の上界）を f64 の四則と sqrt で求める。各サブパスの `ceil(長さ / 周期) * 周期内の「オン」要素数` に、開始点をまたぐ途中の破線と終点の点線の余裕分を加えて合算する。量子化で周期が短くなる場合に備え、Q8〜Q32 で取り得る最短の周期と最大の余裕分を使う。制御点の量子化による長さの増加も見込む。合計が 4096（`MAX_DASHES_PER_PATH`）を超えたら `INVALID_ARGUMENT: dash count`。`validate_paths` はレイヤー合計の上界も 16384（`MAX_DASHES_PER_LAYER`（新規））で拒否する。上界は保守的なので、本当の数が上限以下でも拒否されることがある。
- 実行時の安全網：整数の折れ線の上で実際に数えた破線が 4096 を超えたら `RasterError::CurveRange`（全タイルでそのパスの線を飛ばす。整数の曲線分割による差もこの上限で扱う）。

**アルゴリズム。**`stroke_q8.rs` の `outline` の中、`flatten_stroke` の直後に入れる（`dash_polylines`（新規））。

1. 量子化：パターンの各要素と offset を、幅と同じ `raster_q8::q_from_f32_bits(x.to_bits(), bits)` でローカルの Q にする。すべての要素が 0 に丸まる場合はローカル座標を Q10 に上げる。偶奇：奇数個は2回繰り返す。周期 `P` はその合計。`phase = offset_q.rem_euclid(P)`。パターンを `phase` だけ進めた位置から始める（正の offset はパターンを後ろへずらして、途中から始める。SVG の `stroke-dashoffset` と同じ）。
2. 長さ：折れ線の各線分の長さは既存の `length`（整数の平方根の切り捨て）。長さ0の線分は無視する。累積はすべて i64。
3. 分割：各線分の中の位置 `pos`（`0 < pos < L`）の点は `a + div((b - a) * pos, L)`（成分ごと、`div` は0から遠い側への丸め（half-away））。`pos = 0` は `a`、`pos = L` は `b` をそのまま使い、誤差を累積しない。「オン」の区間は、複数の線分をまたいでも**1本の破線**（線分の列）にして、中の頂点には結合を作る。分割で生じた線分は元の線分の `tangent` と `normal` を引き継ぐ（`b - a` から計算し直さない）ので、同じ線分から分かれた断片のあいだに結合は出ず、継ぎ目に隙間もない。
4. 長さ0のオン（例：`[0, 8]` と round の線端で点線）：その位置の元の線分の向きで、長さ0の線分（`a == b`）1本の破線にする。本体の四角形は面積0で `polygon` が捨て、両端の線端だけが残る。round は円、square は一辺が線幅の正方形、butt は何も描かない。向きは長さ0の線分から取れないので、`tangent` を明示する（上の引き継ぎと同じ仕組み）。
5. 閉じたサブパス：折れ線全体が最初の「オン」の区間に収まる（破線が全周を覆う）ときは破線化せず、従来どおり閉じた線として結合まで作る。それ以外は、各破線を開いた線（`closed = false`）として扱い、両端に線端を付ける。閉じ点をまたぐオンの最後の破線と最初の破線は**つなげない**（SVG の仕様は未規定で、主要な実装もつなげない）。`Close` が作る最後の線分も他と同じく分割の対象。
6. 出力：各破線を、既存の「線分の列から本体・線端・結合の多角形を作る」処理（`outline` の内側の本体）へ渡す。この処理を `fn stroke_polyline`（新規）へ切り出し、破線なしの経路と共有する（破線なしの出力は1バイトも変えない）。

**他の経路。**

- `vector_pixels_q14` は `rasterize_stroke` → `outline` を呼ぶので、追加の変更は無い（IMG-2 の確定も破線を含む）。
- legacy の lyon は破線を持てないので、`dash` を見つけたら `VectorError` を返す。
- `canvas.rs` の `vector_extent` は線幅から余白を求めるだけで、破線で範囲が広がることはない。変更不要。
- `canvas.resize` の更新は不要：破線の長さはノード座標で、キャンバス編集はルートの変形だけを書き換える（`canvas.rs`）。
- 精度：折れ線への分割の細かさは `bits`（ズームのレベルで変わる）に依存するので、破線の位相は、曲線の分割誤差の範囲（レベルごとに画素より十分小さい）でレベルによって動く。レベル0の画素が正本で、これは既存の線の輪郭の精度（`curve_precision_tracks_the_zoom_level`）と同じ性質である。

**SDK。** `types.ts` の `VectorStroke` に `dash?: { pattern: number[]; offset?: number }`。

### 形式と通信

- `format_version`：**上げる**（7。VEC-1 と同じ回）。`dash` は `deny_unknown_fields` の型に増え、古い実装は読めない。`dash` なしの文書のバイト列は変わらない。
- パッケージの manifest の `document_format` はスナップショットの形式を使う。読み込みで参照する `alto_doc::SUPPORTED_FORMATS` に 7 を加え、既存の 5 と 6 も受け入れる。破線と形式6の書式が同居するときは形式7を使う。
- `PROTOCOL`：上げない（要求の追加なし）。
- 描画の版：v3 のまま。破線なしの画素は変わらない（共通規則1）。破線の画素は新規。
- シェーダー：変更なし（輪郭は CPU が作る）。

### 作業手順

**Files:** Modify `crates/alto-store/src/lib.rs`（VEC-1 と同じ番号を共有。先に入れた側が変える）、`crates/alto-vector/src/lib.rs`、`crates/alto-gpu/src/stroke_q8.rs`、`crates/alto-gpu/src/raster_q8.rs`（必要なら `flatten_stroke` の公開範囲のみ）、`crates/alto-gpu/src/cpu_composite/v3_tests.rs`、`crates/alto-doc/src/version.rs`（`style` の判定）、`crates/alto-engine/src/vector_tests.rs`、`packages/alto-sdk/src/types.ts`、`packages/alto-sdk/api/alto-engine.api.md`。

**Interfaces:** Produces `Stroke.dash`、`Dash`、`Dash::validate`、`stroke_q8::dash_polylines`、`stroke_q8::stroke_polyline`。

- [ ] 手順1：`stroke_polyline` を切り出すだけの変更。`no_dash_outline_is_byte_identical`（既存の線のケース数本で、変更前後の `outline` のコマンド列が同じ）が通ること。
- [ ] 試験（`alto-vector`）`no_dash_serializes_the_same_bytes`（既存の `fingerprint` が不変）、`dash_roundtrips_through_json`、`all_zero_pattern_is_refused`、`dash_pattern_longer_than_16_is_refused`、`negative_dash_length_is_refused`、`dash_past_the_count_limit_is_refused`（パス単位とレイヤー合計）、`odd_pattern_validates_as_its_double`。
- [ ] 試験（`stroke_q8.rs`、整数の厳密値）`dash_splits_a_line_at_the_pattern`（`[10, 5]` の水平線で破線の区間が `0..10`、`15..25` …）、`dash_offset_shifts_the_phase`（`offset = 3` と負の offset）、`odd_pattern_repeats_twice`、`dash_runs_across_segment_boundaries_as_one_dash`（折れ線の頂点に結合がある）、`dash_pieces_from_one_segment_leave_no_seam`（分割点でカバレッジに欠けがない）、`zero_length_dash_makes_a_dot_with_round_and_square_caps_only`、`closed_path_dashes_over_the_close_segment`、`closed_path_covered_by_one_dash_stays_closed`、`dash_on_a_curve_follows_the_flattened_length`、`dash_count_over_the_limit_skips_the_stroke_on_every_tile`（`CurveRange`、窓に依存しない）、`dash_is_independent_of_the_window`（窓をずらしても同じ被覆）。
- [ ] 試験（`cpu_composite/v3_tests.rs`）`v3_dashed_stroke_matches_the_integer_reference`（GPU と `rasterize_stroke` の参照が一致）、`v3_dashed_stroke_follows_the_node_transform`（回転と非一様拡大）。
- [ ] 試験（`alto-doc`）`diff_reports_a_dash_change_as_a_style_change`。`canvas.rs`：`dash_survives_canvas_resize`。
- [ ] SDK の型、API レポート、`pnpm --dir packages/alto-sdk build`。
- [ ] 完了条件：破線なしの全既存試験と `tests/parity/` が不変。`sh scripts/check.sh native`、`node scripts/verify-browser.mjs --functional` が成功。opencode レビュー後に PR。

### 依存

- 先に：`ContentDetail::Vector.style`（`crates/alto-doc/src/version.rs`）は VEC-1 が足す。VEC-2 が先に入る場合は VEC-2 が足し、VEC-1 は再利用する（先に着手した側が作る）。それ以外は無し。
- 後に：VEC-3 段階3（`stroke-dasharray`）、IMG-2（破線の確定）。VEC-1 とは `format_version` の番号を共有する。

## VEC-3 SVG の CSS、画像、文字、外部参照

### 現状

- 「共通の現状」の SVG の項のとおり。許可する要素と属性は `validate_attrs` の固定表で、`style`、`class`、`defs`、`use`、`image`、`text` はすべて `UNSUPPORTED` で拒否される。名前空間は SVG のものだけ（`xlink` の宣言も拒否される）。
- 入口：`Engine::prepare_svg_import(&self, h, svg, parent, index)` が `PreparedSvgImport { ops, revision, result }` を返し、`image.import_svg` の1トランザクションで適用する（検査用に `Editor::transact` を一度走らせ、失敗は文書を変えない）。`crates/alto-wasm/src/image_versions.rs` の `import_svg`、`packages/alto-sdk/src/worker.ts` の `importSvg`、`doc.importSvg`（`document.ts`）。
- ノード ID は `NodeId::new()`（UUID v7、時刻と乱数）なので、同じ SVG を2回読むと ID は一致しない。決定的なのは木の構造、名前、プロパティである。
- 画像の復号は Rust に無い。`png` は `Cargo.lock` に間接依存としてあるが、`alto-engine` は使っていない。ブラウザー側の `assets.importImage`（`packages/alto-sdk/src/engine.ts`）は `createImageBitmap` で復号して Worker へ渡し、`crates/alto-wasm/src/lib.rs` の `import` が 256 画素のタイルにして `Engine::put_tile`（`&mut self`）と `pin_import` で保持する。上限は辺 32768、67108864 画素（`MAX_IMAGE_PIXELS`、64M）。
- テキストは `Node::text(name, TextContent)`。フォントは文書のアセット（`AssetKind::Font { family, face_index, embedding }`）を `TextStyle.fonts: Vec<AssetId>` で参照する（最大16、`MAX_FONTS`）。点テキストの原点は 1行目の行ボックスの上端で、基線は組版結果の `LineBox.baseline`（`crates/alto-text/src/layout.rs`）。
- 試験：`svg.rs` 内の `rejects_unknown_attributes_external_content_and_partial_import` が `<text>`、`style="fill:red"`、`<use href="https://…">`、DTD の4本を拒否の例にしている。fuzz target `svg`（`fuzz/fuzz_targets/svg.rs`）と、回帰の `crates/alto-engine/tests/fuzz_regressions.rs`。

### 11-04 からの変化

- `svg.rs` は変化なし（b2036ce 以降のコミットが無い）。ただし円弧の境界は 754ae49 で `PathData::from_svg`（`alto-vector`）側に入った（`check_svg_arcs`）。SVG の `d` は `svg.rs` の `scaled_path` と `path` を通り、この関数は通らない。
- QA-1 が実装され（0e6f491）、`svg` の fuzz target と seeds（`fuzz/seeds/svg/`）がある。11-04 の「QA-1 のファザーに含める」は、新しい段階のたびに seeds と回帰の試験を足す作業になる。
- `svg.rs` の中身ではなく周辺が変わった点：DocumentMeta は format 5・render v3 のみ。ルートの変形は `canvas.rs` が書き換える。

### 設計

原則：非対応は部分 import せず拒否する（`UNSUPPORTED`、バイト・行・列付き）。上限超過は `RESOURCE_LIMIT`。ネットワークとファイルシステムには一切出ない。段階ごとに PR を分け、段階ごとに fuzz の seed を足す。

新しいコードは `svg.rs` を肥大させないよう、同じ階層の別ファイルへ分ける（`crates/alto-engine/src/svg_style.rs`（新規）、`svg_paint.rs`（新規）、`svg_use.rs`（新規）、`svg_image.rs`（新規）、`svg_text.rs`（新規）。`lib.rs` に `mod` を足す）。`svg.rs` は入口として残し、ファイル名と位置は変えない（他の文書が参照している）。中身（`prepare_svg_import` のシグネチャと各段階の呼び出し）は変える。

**段階1：`style` 属性と `<style>` の単純セレクタ。**

- `<style>`（`type` は無いか `text/css`、`media` は拒否）はどこにあっても事前に集め、木の歩行では描画しない。1文書の CSS は合計 64 KiB、規則 1024、1規則の宣言 64 まで（`RESOURCE_LIMIT`）。
- 文法（自前の小さな解析器。外部クレートは足さない）：セレクタは型、`.class`、`#id` の1つだけ（カンマ区切りの並びは可）。結合子、属性、擬似クラス、`*`、`@` 規則、`!important`、`url()`（`fill`・`stroke` の `url(#id)` を除く）、コメント以外の未知の構文は位置付きで拒否する。
- プロパティは既存の属性と同じ集合に、段階3の `stroke-dasharray`、`stroke-dashoffset` と段階2の `stop-color`、`stop-opacity` を足したもの。`transform`、幾何属性、`display` などは CSS からは設定できない（拒否）。
- 優先順位（SVG の規則）：表示属性 < スタイルシート < `style` 属性。スタイルシート同士は詳細度（id > class > 型）、同じなら後のものが勝つ。継承は既存の `Style` の受け渡しと同じ。
- `class` 属性を許可する（空白区切り）。重複する `id` は拒否する（`<use>` と勾配の参照を一意にするため）。

**段階2：`<linearGradient>`、`<radialGradient>`（VEC-1 に写す）。**

- `<defs>` を許可（子は描画しない）。`fill="url(#id)"` と `stroke="url(#id)"`（文書内の `#id` のみ。ほかの `url(...)` は拒否）。
- 属性：線形は `x1 y1 x2 y2`（既定 0%,0%,100%,0%）、放射は `cx cy r`（既定 50%）。`fx fy fr` が中心・半径と違うなら拒否（焦点つき放射は VEC-1 の範囲外）。gradientUnits は userSpaceOnUse と objectBoundingBox（既定）の両方。gradientTransform は `Gradient.transform` へ。spreadMethod は `pad` のみ（`reflect`、`repeat` は拒否）。
- objectBoundingBox：対象のパスの `PathData::bounds()`（線の太さを含まない幾何の範囲）で `[x0 + u*(x1-x0), y0 + v*(y1-y0)]` に写す行列を `transform` の左に合成する。幅か高さが0なら（SVG の規則では描かない図形だが）部分 import をしない原則に従い `UNSUPPORTED` で拒否する。`%` 単位は objectBoundingBox のときだけ許す。
- `<stop offset stop-color stop-opacity>`：offset は 0..=1（`%` 可）で、SVG の規則どおり前の値より小さければ前の値にそろえる。2〜16個（超過は `RESOURCE_LIMIT`）。`stop-opacity × fill-opacity`（線は `stroke-opacity`）を停止点のアルファにする。1個の停止点は単色として扱う（VEC-1 の下限 2 個のため、同じ色を2個に展開する）。0個は拒否。
- `href`（SVG 2）と `xlink:href` による勾配の継承は、同じ文書の `#id` だけを許す。`xlink` の名前空間宣言はこの用途に限って許す（ほかの名前空間は引き続き拒否）。継承は深さ 8 まで、循環は拒否。

**段階3：`stroke-dasharray`、`stroke-dashoffset`（VEC-2 に写す）。** `none`、または数値のリスト（コンマまたは空白区切り、長さは `px` か単位なし）。`%`、`em` などは拒否。負の値は拒否。合計が0は SVG の規則どおり `none`（`dash: None`）。要素数が16を超えれば `RESOURCE_LIMIT`。上界の検査（VEC-2）に落ちたら `RESOURCE_LIMIT`。

**段階4：`<image>`。**

- `href` は `data:image/png;base64,…` または `data:image/jpeg;base64,…` のみ。`x y width height` と `preserveAspectRatio` は `none` と既定の `xMidYMid meet` だけ。`transform` と合わせて、ラスターレイヤーのノード変形に写す。ラスターの描画は他のラスターレイヤーと同じ（v3 の最近傍の参照）で、拡大縮小は再サンプルしない。
- 復号は Rust（既定）：PNG は既に `Cargo.lock` にある `png`（純 Rust）。JPEG は新しい依存が要る（案：`zune-jpeg`）。**利用者の決定が要る点**：JPEG を Rust で復号する（WASM が増える。`doc/MILESTONES.md` の WASM サイズの記録に追記が要る）か、JPEG は拒否して PNG のみで始めるか。既定は PNG のみ、JPEG は決定後。SDK 側（`createImageBitmap`）で復号して Worker へ渡す案は、ブラウザーごとの色変換で画素が変わる（決定性に反する）ので採らない。
- 上限：復号の前にヘッダーの寸法を読み、辺 32768 かつ画素数 ≤ `MAX_IMAGE_PIXELS`（64M）を確かめる。1回の import の合計も 64M 画素まで。画像は 64 個まで。`data:` の本体は SVG 全体の 4 MiB の中に収まる。色の管理は無く、gAMA と iCCP の chunk は無視して sRGB とみなす（COL-5 まで）。
- 取り込み：`prepare_svg_import` は `&mut self` にする（`put_tile` が要る）。タイルを `put_tile` と `pin_import` で保持し、`TileMap` を持つ `Node::raster` と、元のバイト列の `Op::SetAsset`（`EncodedImage`）を同じトランザクションに入れる。`pin_import` は `Engine.imported` への全体共通の追加で、import ごとの解放手段が無く、`release_import` は保留中のブロブ用である。そこで、(a) 復号、検証、`Editor::transact` の検査をすべて先に行い、`put_tile` は最後に行う、(b) 呼び出し元の失敗（版の競合など）に備えて `Engine::unpin_imports`（新規）を作る（この import が新しく加えた `imported` のハッシュだけを外し、すでに固定されていたものは残す）を作り、`PreparedSvgImport` に新しく固定したハッシュの一覧を持たせる。`crates/alto-wasm/src/image_versions.rs` の `import_svg` は既に `&mut self`。
- `data:` 以外（`http:`、`file:`、相対パス、`blob:`）は常に拒否する。

**段階5：`<text>`。**

- `<text x y font-family font-size fill>` と1個のテキストノードだけ。`<tspan>`、`<textPath>`、`text-anchor` の `start` 以外、`font-weight`・`font-style` の `normal` 以外、`dx`・`dy`・`rotate` は拒否する。文字数は `TextContent` の上限（1レイヤー16384 UTF-16 単位）に従う。
- `font-family` は文書のフォントアセット（`AssetKind::Font.family`）で解決する。引用符を外し、完全一致（ASCII の大文字小文字は区別しない）。リストの先頭から順に解決できたものを `TextStyle.fonts` に並べる（最大16）。総称名（`sans-serif` など）は無視する。**リストのどれも解決できなければ拒否**する。同じ family のアセットが複数あれば曖昧として拒否する。
- 位置：SVG の `y` は 1行目の基線なので、組版を1回実行して 1行目の `baseline`（`LineBox.baseline`）を得て、ノードの平行移動を `(x, y - baseline)` にする。文字色は `fill`（勾配は拒否。文字は VEC-1 の対象外）。
- 取り込みは `Node::text` ＋ `Op::SetText` の通常の経路で、焼き込み（`bake`）は他のテキスト編集と同じに文書の適用後に行われる。

**段階6：外部参照と `<use>`。**

- `<use href="#id">`（と `xlink:href="#id"`）の文書内参照だけを展開する。`href` が `#` で始まらないものは常に拒否（`http:`、相対ファイル、`data:`、別文書の `file.svg#a`）。`<symbol>`、`<marker>`、`<pattern>`、`<mask>`、`<clipPath>`、`<filter>`、`<foreignObject>`、`<script>`、`<a>` は引き続き拒否。
- 展開：参照先の要素（図形か `g`）の木を、`use` の `x`、`y`（`translate`）と `transform` を持つ新しいグループの下へコピーして作る。スタイルは `use` の位置から継承する（CSS の継承の規則）。`<defs>` の中の要素は直接は描画せず、参照されたときだけ使う。
- 循環の検出：展開中の `id` の経路を持ち、同じ `id` が経路に再び現れたら拒否（参照の自己・相互循環）。深さ 16。展開は既存の `Builder` の上限（ノード 10000、合計命令 65536）を、コピーを作るたびに数えて、超えた瞬間に `RESOURCE_LIMIT` で止める（指数的な展開は上限で止まり、メモリを先に使い切らない）。
- 決定性：コピーの順序は文書順、名前は元の `id`（無ければ要素名）。`NodeId` は UUID v7 でそもそも決定的ではないので、決定性の定義は「構造・名前・プロパティが一致すること」とする。

XML の展開攻撃には、`roxmltree` の `allow_dtd: false`（エンティティ展開なし）と `nodes_limit` を維持して備える。新しい入力経路（CSS、`data:` URI、`href`、勾配の継承）はすべて fuzz の対象にする。

### 形式と通信

- `format_version`：上げない。生成するノードは既存の種類（グループ、ベクター、ラスター、テキスト）と既存の `Op` だけである。ただし段階2・3は、VEC-1・VEC-2 が先に入っていること。
- `PROTOCOL`：上げない。`importSvg` の要求と結果は変わらない（フォントと画像は文書とデータ URI から得る）。`prepare_svg_import` の Rust のシグネチャは `&mut self` になる（段階4）。
- 描画の版：v3 のまま、新しい画素の計算なし。
- シェーダー：なし。

### 作業手順

**Files:** Modify `crates/alto-engine/src/svg.rs`、`crates/alto-engine/src/lib.rs`（`mod` の追加、`unpin_imports`（新規））、`crates/alto-engine/tests/fuzz_regressions.rs`、`fuzz/fuzz_targets/svg.rs`（段階4・5で、フォントのアセットを持つ文書で呼ぶ）、`fuzz/seeds/svg/`（段階ごとに追加）、`crates/alto-wasm/src/image_versions.rs`（段階4で `prepare_svg_import` の呼び出し）、`crates/alto-engine/Cargo.toml`（段階4で `png`、決定後に JPEG の復号器）、`packages/alto-sdk/test/`（SDK からの取り込みの試験）。Create `crates/alto-engine/src/svg_style.rs`（新規）、`svg_paint.rs`（新規）、`svg_use.rs`（新規）、`svg_image.rs`（新規）、`svg_text.rs`（新規）。段階ごとに PR。

**Interfaces:** Consumes VEC-1 の `Paint`、`Gradient`、VEC-2 の `Dash`。Produces 位置付きの `UNSUPPORTED`／`RESOURCE_LIMIT` で拒否する CSS・勾配・画像・テキスト・`use` の取り込み。

- [ ] 段階1の試験 `style_attribute_overrides_presentation_attribute`、`stylesheet_overrides_presentation_attribute_and_style_attribute_wins`、`specificity_orders_id_over_class_over_type`、`unsupported_selector_is_refused_with_position`（子孫、`:hover`、`[a]`、`*`）、`important_and_at_rules_are_refused`、`css_over_the_size_limit_is_refused`、`css_cannot_set_transform_or_geometry`。既存の `rejects_unknown_attributes_external_content_and_partial_import` から `style="fill:red"` を外し、対応表を更新する。
- [ ] 段階2の試験 `svg_gradient_maps_to_paint`、`object_bounding_box_units_map_to_node_coordinates`（非正方形の bbox の放射が楕円になる）、`gradient_transform_maps_to_gradient_transform`、`spread_method_reflect_is_refused`、`focal_radial_is_refused`、`stop_opacity_and_fill_opacity_multiply`、`single_stop_gradient_becomes_a_solid_pair`、`gradient_href_inherits_stops_and_refuses_cycles`、`url_to_a_missing_gradient_is_refused`、`external_gradient_url_is_refused`。
- [ ] 段階3の試験 `dasharray_maps_to_dash`、`odd_dasharray_keeps_its_length`、`zero_sum_dasharray_is_none`、`percent_dasharray_is_refused`、`dasharray_over_the_count_limit_is_a_resource_limit`。
- [ ] 段階4の試験 `png_data_uri_becomes_a_raster_layer`（寸法、位置、`SetAsset`）、`external_image_href_is_refused`、`image_over_the_pixel_limit_is_refused_before_decoding`（小さい PNG のヘッダーだけが巨大）、`images_over_the_total_pixel_budget_are_refused`、`failed_import_unpins_its_new_tiles`（失敗後の `imported` が import 前と同じ。既に固定済みのタイルは残る）、`stale_revision_after_prepare_can_unpin`、`corrupt_png_is_refused_with_position`、`non_data_image_uri_is_refused`（`http:`、`file:`、相対、`blob:`）。
- [ ] 段階5の試験 `text_resolves_font_family_from_document_assets`、`unknown_font_family_is_refused`、`font_family_list_uses_the_first_resolvable_and_ignores_generics`、`ambiguous_font_family_is_refused`、`text_baseline_matches_the_svg_y`、`tspan_and_textpath_are_refused`。
- [ ] 段階6の試験 `use_expands_a_same_document_reference`、`use_cycle_is_refused`（自己・相互）、`use_expansion_past_the_limit_is_refused`（入れ子で指数に増える例が `RESOURCE_LIMIT` で止まり、止まるまでのメモリが有界）、`http_use_href_is_refused`、`relative_file_use_href_is_refused`、`use_inherits_style_from_the_use_site`、`use_import_is_structurally_deterministic`（2回読んで `NodeId` を除いた木が同じ）、`duplicate_ids_are_refused`。
- [ ] すべての段階：新しい入力ごとに `fuzz/seeds/svg/` へ seed と、見つかった入力の回帰（`fuzz_regressions.rs`）を足す。`cargo +nightly fuzz run svg` を短時間回す手順は 13-verification.md の QA-1 に従う。
- [ ] 完了条件：拒否した取り込みは `snapshot` が変わらない（`rejects_…` の形式）。`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional` が成功。段階ごとに opencode レビューと PR。

### 依存

- 先に：段階2は VEC-1、段階3は VEC-2、段階5は 06-text.md の組版が安定していること（TXT-5、TXT-6 などで `TextContent` が増える場合、`<text>` の写し先を合わせる）、段階4の復号は依存の決定（JPEG）。段階1と段階6は独立。
- 後に：なし。QA-1 の fuzz target `svg` は段階ごとに seed を増やして使う。直接依存の `png` crate は本項目が足し、COL-5、COL-2（01）が共有する（00-ROADMAP の重なりの表）。

## VEC-4 vello 本体描画

### 現状

- v3 の本体は整数経路（「共通の現状」）で、全バイト一致が定義である。vello の浮動小数の結果は正本にできない。
- vello は比較用の `vello-probe` feature だけ：`crates/alto-wasm/Cargo.toml`（`vello = "=0.11.0"`、`wgpu = "30.0.1"`）、`crates/alto-wasm/src/vector_probe.rs`（`compareVectorBackends`、`verifyVectorOptimization`）、ビルドは `scripts/build-vector-comparison.sh`（`--features vello-probe` で**エンジン全体**の WASM をもう一つ作る）、ページは `examples/vector-comparison/`。`alto_vector::VectorRasterizer` の `prepare` を lyon と vello（`VelloPreparation`）が実装している。
- リリースのパッケージは vello を含まない：`scripts/check.sh` が `alto_wasm.d.ts` に `compareVectorBackends` と `verifyVectorOptimization` が無いことを検査する（d2718f1）。
- 比較結果 `doc/verification/VECTOR-COMPARISON.md`：100パス、512×512、warm の中央値は lyon（旧 float の経路）25.9 ms、vello 0.8 ms。**比較相手は lyon で、現在の整数経路ではない。**
- 整数経路の速度：`doc/verification/ENGINE-HARDENING-RESULTS.md` の text-vector（3840×2160、125パスのベクター層8枚）で fit のコールド表示は、E-7 の約8秒を経て、最後の記録（E-10）で 2187〜2267 ms（最初の main は約61秒）。被覆の CPU 計算が支配的（`vector_q14` の自己時間、`raster_q8` の線分処理）。
- 表示の段階：`DisplayReport.phase`（0 準備、1 保持、2 粗い／精緻化、3 exact）と `requested_level`、`admitted_level`（`crates/alto-gpu/src/renderer/presentation.rs`）。タイルのキャッシュは `crates/alto-gpu/src/renderer/cache.rs`。
- `EngineOptions`（`packages/alto-sdk/src/engine.ts`）に `gpu: { powerPreference, memoryBudgetMB }` はあるが、ベクターの経路を選ぶ設定は無い。

### 11-04 からの変化

- 判定の基準が変わった。11-04 は「VECTOR-COMPARISON と同じ条件で、整数経路の2倍以上速くなければ」と書いたが、その比較は旧 float の lyon 経路との比較で、整数経路との比較ではない。整数経路は堅牢化 E の後に大きく速くなり（fit のコールドが約61秒から E-10 で約2.2秒）、PERF-1〜3 でさらに変わる。判定は**整数経路に対して新しく測り直す**。
- 「別の WASM パッケージ」の実体は、エンジン全体を feature 付きで作り直した WASM である（`scripts/build-vector-comparison.sh`）。vello の `Scene` を描く wgpu のデバイスとテクスチャは WASM モジュールごとに別物で、二つのモジュールのあいだで GPU のテクスチャを渡す仕組みは確認していない。したがって、本体 WASM から vello 専用の小さな WASM を呼ぶ設計は採らず、**選んだときだけエンジン全体の別ビルドを読み込む**。
- `check.sh` に、リリースが vello の公開関数を含まない検査が入った。VEC-4 の別ビルドはこの検査を保ったまま作る（同じ名前を使わないか、検査の対象に別ビルドを含めない）。

### 設計

**先に測る（判定）。** 実装の前に、vello を表示のプレビューに使う価値があるかを測って記録する。

- 計測ページを `tests/diagnostics/vector-preview.html`（新規）に作る（`tests/diagnostics/vector-optimization.html` と同じ構成）。同じ密集ベクター文書（text-vector と同じ生成。125パス × 8層、3840×2160）で、(A) 現在の整数経路、(B) vello の `Scene` 描画を、等倍と fit のコールド・warm の両方で測る。入出力の変換（vello の出力から Q14 のタイルへ）の時間も (B) に含める。
- 判定：(B) が (A) のコールドと warm の両方で2倍以上速くなければ、実験的な選択肢のまま（公開の `createEngine` 項目にはせず、`unstable` 名前空間（`packages/alto-sdk/src/unstable.ts`）にだけ出す）。2倍以上でも、プレビューの見た目の差（アンチエイリアス）が許容できなければ見送る。結果は `doc/verification/` に記録する。速度の約束は書かない。
- PERF-1〜3（タイルのバッチ化、シーン索引、コールド描画）の後に測ると、整数経路が変わるので、測定はそれらの後に行う。

**経路。**（判定を通った場合）

- `createEngine({ vectorPreview: "integer" | "vello" })`（新規）。既定は `integer`。`EngineStatus` に実際に使っている経路（`"integer" | "vello" | "fallback"`）を出す。
- `"vello"` を選ぶと、SDK は vello 入りのエンジン全体の WASM（`alto_wasm` の別ビルド。`vello-preview` feature（新規）、`scripts/build-vector-comparison.sh` と同じ形の `scripts/build-vector-preview.sh`（新規））を Worker で読み込む。既定のビルドは vello を含まず、`wasm/SHA256SUMS` の対象も変えない。別ビルドの配布は `RELEASING` に追記する。
- vello の結果は**プレビュー専用のタイル**として扱う。キャッシュの鍵（`renderer.rs` の `CacheKey`、現状は3整数。印のために型を広げる）と、シーン側の内容（`alto-scene` の `RContent`。プレビュー用の変種か印を足す）にプレビューの印を付け、vello で描いたタイルを入力にした合成結果もプレビューの印を引き継ぐ。印のあるタイルは、読み戻し（`exportImage` など）、ハッシュ（`snapshotHash` 系）、保存用のプレビュー、parity、IMG-2 の確定のいずれにも**使わない**（それらは常に整数経路の exact を要求する）。
- vello の出力（straight アルファ）は、専用の小さな変換 shader（`crates/alto-gpu/shaders/vello-import-q14.wgsl`（新規））で premultiplied の Q14 にして、ベクターノードの `source` として合成の段へ入れる。以後のマスク、効果、ブレンドは通常の Q14 の段が行うので、プレビュー専用の合成は作らない。
- ビューが止まったら（`DisplayReport` が精緻化の段階に入ったとき）、見えているプレビューのタイルを整数経路で描き直して置き換える。置き換えが終わるまで表示は `phase = 2`（粗い）として報告する。`phase = 3`（exact）はプレビューを含まない。
- 失敗時は整数経路へ戻る：vello 入り WASM の読み込み失敗、GPU デバイスの喪失（`GPU_LOST`）、vello の描画エラーのいずれでも、プレビューのキャッシュを捨てて整数経路で描き、`EngineStatus` を `"fallback"` にする。文書と Undo は影響を受けない。

### 形式と通信

- `format_version`：触れない（保存データと編集 Op は描画の経路から独立）。
- `PROTOCOL`：Worker の初期化要求に `vectorPreview`（新規）が増えるなら**上げる**（16。段階4の回）。README と RELEASING を同じ PR で更新する。
- 描画の版：v3 のまま。正本の画素は変えない（共通規則1）。vello は表示だけの別経路で、parity の対象外。
- シェーダー：`vello-import-q14.wgsl` は効果のモード番号を使わない（ノード内容の入力変換）。

### 作業手順

**Files:** Create `tests/diagnostics/vector-preview.html`（新規）、`scripts/build-vector-preview.sh`（新規）、`crates/alto-gpu/shaders/vello-import-q14.wgsl`（新規）、`doc/verification/VECTOR-PREVIEW.md`（新規）。Modify `crates/alto-scene/src/lib.rs`（`RContent` にプレビュー用の内容とプレビュー印の持ち方を足す。印は描画シーンに無いので必須）、`crates/alto-gpu/src/renderer.rs`（`CacheKey` は `(i32, i32, i32)` の3整数で、印を入れるには型を広げる）、`crates/alto-wasm/Cargo.toml`（`vello-preview` feature）、`crates/alto-wasm/src/lib.rs`、`crates/alto-gpu/src/renderer/tile_renderer/graph.rs`、`crates/alto-gpu/src/renderer/cache.rs`、`crates/alto-gpu/src/renderer/presentation.rs`、`scripts/check.sh`（別ビルドの検査）、`packages/alto-sdk/src/engine.ts`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/api/alto-engine.api.md`。

**Interfaces:** Produces `EngineOptions` の `vectorPreview` 項目、`EngineStatus` の経路表示、プレビュー印つきのタイルキャッシュ鍵。

- [ ] 手順1：計測ページで (A)、(B) を測り、`doc/verification/VECTOR-PREVIEW.md`（新規）に条件（機種、Chromium の版、文書の生成式、回数、中央値）と判定を記録する。判定が「見送り」なら、ここで終わりにして状態を「実験的のまま」と書く（以降の手順は行わない）。
- [ ] 試験 `preview_tiles_are_never_read_back`（書き出し、確定、ハッシュが常に整数経路の結果と一致する。プレビュー印のあるタイルを読み戻しの経路が拒否する）。
- [ ] 試験 `preview_mark_propagates_through_composition`（プレビューのタイルを入力にしたブレンド・効果・マスクの結果にも印が付く）。
- [ ] ブラウザー試験 `a settled view replaces preview tiles`（止めた後の画素が整数経路と全バイト一致）、`the default build has no vello`（既定の `alto_wasm.d.ts` と WASM に vello の関数が無い。`check.sh` の検査が保たれる）、`the vello build is loaded only when selected`。
- [ ] GPU 喪失と、別ビルドの読み込み失敗で整数経路へ戻ること、文書と Undo が無傷なことを試験（`GPU_LOST`、読み込み失敗の注入）。
- [ ] 完了条件：`tests/parity/` の既存行が不変。`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional` が成功。opencode レビュー後に PR。

### 依存

- 先に：PERF-1〜3（整数経路の速度が変わるため、判定はその後）、RUN-4（readback プールの整理と競合しうる）。
- 後に：なし。

## Review Focus

- 破線：分割した破線が、線端（cap）の扱いで既存の閉じたパスと矛盾しないこと（全周を覆う破線は閉じた線のまま、閉じ点をまたぐ破線は2本）。分割点に継ぎ目の隙間が無いこと。
- 勾配：レイヤー変形とマスクの独立変形の両方で、パスと一緒に動くこと。`canvas.resize` は勾配の座標も破線の長さも書き換えない（ルートの変形が動く）ので、書き換えを足してしまわないこと。
- 勾配：`uniform.rs` の一様サブツリーの近道が、勾配の塗りで使われないこと。
- 差分の表示：勾配と破線の変更が `ContentChanged` の `style` として見えること。
- SVG：`<use>` の展開が上限で止まり、構造・名前・プロパティが決定的であること（`NodeId` は UUID v7 で、そもそも決定的ではない）。外部への参照が常に拒否され、読み込みでネットワークにもファイルにも出ないこと。拒否した取り込みが文書を変えないこと。
- 経路の取り違え：線は `stroke_q8` を通るが、ブラシのストロークは通らない。ベクターの着色は `glyph-q14.wgsl` で、`vector-q14.wgsl` ではない。lyon と vello は v3 の経路に無い。
