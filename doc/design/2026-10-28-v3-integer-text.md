# v3 整数テキスト

作成日：2026-10-28（Asia/Tokyo）

状態：設計のみ。この文書は実装しない。テキストの v3 は、[v3 整数合成](2026-10-28-v3-integer-compositing.md)（PR [#24](https://github.com/NobukiAkiyama/alto-engine/pull/24)、main の `5ce300e`）と [v3 整数マスク](2026-10-28-v3-integer-mask.md)（PR [#26](https://github.com/NobukiAkiyama/alto-engine/pull/26)、main の `7309b61`）と同じ不安定オプトインの向こうに置く。描画バージョン 1 と 2 のテキストの式（f32 のアトラス、4x MSAA、ハードウェアのブレンド）は凍結したままである。

関連：[v3 整数合成](2026-10-28-v3-integer-compositing.md)、[v3 整数マスク](2026-10-28-v3-integer-mask.md)、[正確な CPU 参照](2026-10-27-exact-cpu-references.md)、[画像編集](IMAGE-EDITING.md)、[半透明合成の計測](../verification/semi-transparent-composite-parity.md)。

## 前提

テキストは合成パス（`composite.wgsl`）を通らない。`raw_node`（`crates/alto-gpu/src/renderer/m5.rs`）の `RContent::Text(runs)` が `vector`（同ファイル）を呼び、`render_text`（`crates/alto-gpu/src/renderer/vector.rs`）が M5 の `rgba16float` のターゲットへ描く。Rust の `gap`（`crates/alto-gpu/src/cpu_composite.rs`）と JS の `sceneGap`（`tests/e2e/cpu-composite.js`）は `"text"` を返し、parity の `text`（`tests/e2e/parity-tests.js` の `xfail("text", …)`）は xfail である。

文字の流れは次のとおりである。

| 段 | 場所 | 今の算術 |
| --- | --- | --- |
| 字形の取り出し | `OutlineCache::outline`（`crates/alto-text/src/outline.rs`）。`Collect` が `ttf_parser` の `OutlineBuilder` を受ける | `ttf_parser` の `outline_glyph`。ヒンティングなし。座標は f32 のフォント単位を f64 にしたもの。TrueType（`AltoTestLatin.ttf`）は整数の二次、CFF（`AltoTestJP.otf`）は f32 で解釈した三次 |
| 整形と配置 | `layout`（`crates/alto-text/src/layout.rs`、rustybuzz）。配置行列は同ファイルの `place` | rustybuzz の位置は整数のフォント単位。行列は f64 の加減乗除だけ（`size / f.upem`、`s * k` など）。`hypot` も超越関数もない |
| ラン | `glyph_runs`（`outline.rs`）、`TextRun` / `TextGlyph` / `GlyphOutline`（`crates/alto-vector/src/glyph.rs`） | `placed` が `CHUNK`（4000 コマンド）と `LIMIT`（1_048_576）で外す |
| 帯の選択 | `render_text` | `em_px = run.em * max_scale(world) / scale`。`em_px >= ATLAS_MAX_EM_PX`（24）か `use_atlas` が偽なら、ランごと `run.paths` を 4x MSAA のベクター経路へ |
| 配置 | `place`（`crates/alto-gpu/src/glyph_atlas.rs`） | 行列を `STEPS`（1/4 px/em）の帯に、原点の端数を `PHASES`（1/4 px）に、f64 の `round` で量子化する。ビットマップは境界 + 1 画素のパディング、辺は `MAX_GLYPH_PX`（96）まで |
| ラスタ化 | `rasterize` と `accumulate`（`glyph_atlas.rs`） | **CPU**。曲線は `hypot` と `sqrt` で分割数を決め、f64 で評価する。符号付き面積を f32 の `acc` に足し、行優先の f32 の走行和 `sum` の `(sum.abs().min(1.0) * 255.0 + 0.5) as u8` を R8 にする |
| アトラス | `GlyphAtlas`（`glyph_atlas.rs`）。テクスチャは `render_text` が作る `R8Unorm`、`ATLAS_SIDE`（1024）四方 | 棚の LRU。満杯で現在のエポックに固定されていれば `insert` が None を返し、その字形は `TextGlyph::to_path` で**ベクター経路**へ落ちる |
| 着色と重ね | `glyph-atlas.wgsl` の `paint`。インスタンスは `GlyphInstance`（`vector.rs`） | `textureLoad(atlas, t, 0).r * v.color.a` の f32 と、`PREMULTIPLIED_ALPHA_BLENDING` のハードウェアブレンドで `rgba16float` へ |

ラスタ化はすでに CPU にある。Metal と lavapipe は native の Rust、SwiftShader は wasm の Rust で同じ `rasterize` を走らせる。f32 と f64 の加減乗除は Rust では融合されず、native と wasm で同じビットになる。今日のずれは次の四つである。

1. **24 px 以上は 4x MSAA。** parity の "Ag" は 72px でここに入る。サンプル位置、ラスタライザの固定小数の精度、辺の結びの規則、resolve の平均と unorm の丸めは、どれも実装が決める。Metal、lavapipe、SwiftShader で一致する理由はない。
2. **`hypot`。** `rasterize` の分割数 `n = (dd * 10.0).sqrt().ceil()` の `dd` は `f64::hypot` である。`hypot` は正しい丸めを要求されず、macOS の libm、glibc、wasm の Rust 同梱の libm は別の実装である。`ceil` の境をまたぐ 1 ULP の差が分割数を変え、カバレッジのバイトを変え得る。稀だが、native と wasm の非決定性である。
3. **着色と重ねが f32 とハードウェア。** `paint` の積、ハードウェアの source-over、`rgba16float` のストア、後段の unorm 解決は、#24 が決定性の基礎にしないと裁定したものそのものである。
4. **出力がキャッシュの状態に依る。** アトラスが満杯なら字形はベクター経路で描かれる。`set_glyph_atlas`（`crates/alto-gpu/src/renderer.rs`、wasm の `setGlyphAtlas`）を切っても同じである。同じ文書の画素が、そのときのアトラスの占有と描画設定で変わる。

JS の参照には文字の経路がない。[正確な CPU 参照](2026-10-27-exact-cpu-references.md) の「text」節が書いた「Rust の f32 と JS の f64 でずれる」は、JS が f64 で同じ式を書いた場合の話である。

[正確な CPU 参照](2026-10-27-exact-cpu-references.md) の「text」節は、24px 未満を Q16 の面積で、72px を vector-fill の Q4 サンプルで、と二つに分けた案だった。この文書がそれを置き換える。理由は三つある。#24 は f16 / unorm の中間を v3 から除いた。vector-fill の Q4 サンプルは別の設計で、まだない。72px の字形も 96 px のビットマップに収まるので、帯ごとに別のラスタライザを持つ理由がない。

## 決定：ラスタ化はすべて CPU、GPU は合成だけ

v3 のテキストは、カバレッジを CPU の整数ラスタライザ一つで作る。GPU は、そのバイトを `textureLoad` で読み、Q14 の整数で着色して重ねるだけである。GPU のラスタライザ、MSAA、サンプラ、ハードウェアのブレンドは、v3 のテキストのどこにも使わない。24 px 以上の字形も同じである。

理由は四つある。

1. **一致の対象が減る。** CPU のラスタライザは Rust の一つのソースで、native（Metal、lavapipe）と wasm（SwiftShader）が同じコードをコンパイルする。整数だけなら、三つのバックエンドの GPU が受け取るアトラスのバイトは定義上同じである。GPU 側に残る差の源は `unorm_byte` の読み戻しだけで、#24 の必須検査がすでに押さえている。
2. **WGSL のラスタライザは割に合わない。** 整数のセル積分は WGSL でも書ける（i32 の `atomicAdd` は可換なので順に依らない）。しかし、曲線の可変長の分割、行ごとの前置和、溢れの分岐を三つ目の実装として持つことになる。三実装のうち最も保ちにくい実装が、いちばん複雑な部分を持つ。速度の利得も小さい。小さい字形はアトラスが再利用し、大きい字形はタイルのキャッシュが内容ハッシュで再利用する。
3. **ハードウェアのラスタ化は仕様で一意でない。** 上の「前提」の 1 である。整数の頂点を渡しても、MSAA のサンプル位置と resolve は実装が決める。
4. **今の形を変えない。** `rasterize` → `R8Unorm` のアトラス → 1:1 のコピー、という構成はそのままで、式だけを整数にする。#24 の「アトラスは unorm のまま、`textureLoad` + `unorm_byte` で読む」もそのまま使える。

したがって、三実装の分担は他の経路と違う。

| 部分 | WGSL | Rust | JS |
| --- | --- | --- | --- |
| 字形の取り出しと整形 | — | 本番（native と wasm で同じコード） | 受け取るだけ（下の「JS の入力」） |
| 配置、Q8 の量子化、分割、カバレッジ、バイト | — | 本番 `glyph_atlas.rs`（参照も兼ねる） | 参照（`cpu-composite.js`、新規） |
| アトラスの読み、着色、重ね | 本番（新しい `glyph-q14.wgsl`） | 参照（`cpu_composite.rs`、テストのみ、新規） | 参照（同上） |

ラスタ化の Rust は参照と本番が同じ関数である。参照として独立なのは JS である。WGSL がラスタ化を持たないことは、「各経路を三度実装する」の例外ではなく、その経路が GPU に存在しないということである。

## 範囲

| 対象 | 対象外 |
| --- | --- |
| テキストノードのカバレッジ（全サイズ、横組・縦組・縦中横・回転・水平比率） | 整形そのもの（rustybuzz、改行、禁則）。整数のフォント単位と f64 の加減乗除で、native と wasm で同じコードである |
| アトラスの鍵、追い出し、満杯時の扱い | テキストノードを親へ載せる合成。M5 の v3（#26 の「実装順」の (c)）と #24 のブレンドの範囲 |
| 着色（文字色の Q14）と、ノード内の字形の重ね | 可変フォント、カラーフォント（COLR、sbix）。main にない（`set_variation` を呼ぶ箇所はない） |
| ノードの Q14 中間（`rgba16uint`）への書き込み | テキストの選択範囲やキャレットの描画。UI であり、タイルに入らない |

## main と #24・#26 から使うもの

新しい丸めは作らない。結びは 0 から遠い側へ行く。

| 名前 | 場所 | ここでの役割 |
| --- | --- | --- |
| `div_q_i64` / `divQBig` | `cpu_composite.rs` / `cpu-composite.js` | 曲線の評価の除算（符号あり）。カバレッジの分割の除算（非負） |
| `div_q`、`Wide`、`wide_mul_u32`、`wide_add` | `crates/alto-gpu/shaders/effects.wgsl` | 着色と source-over。`glyph-q14.wgsl` は別シェーダなので文を写す |
| `unorm_byte` | `effects.wgsl`。Rust の計測側は `cpu_composite.rs` の `UNORM_BYTE_WGSL` | アトラスの `.r` をバイトに戻す |
| `byte_q14` / `byteQ14` | `effects.wgsl`、`cpu_composite.rs`、`cpu-composite.js` | カバレッジのバイトを Q14 にする |
| `div28`（#26） | #24 の 16384² 除算。`(n + 2^27) >> 28`、`n >= 0` | 着色の一回丸め |
| `Gpu::atlas_read`、`atlas_read_shader`、`measureAtlasRead` | `cpu_composite.rs`、`tests/e2e/parity-tests.js` | アトラス読み取りの必須検査。ここで `R8Unorm` を足す |
| `measureRow`、`formatMeasure` | `parity-tests.js` | measured-not-enforced の行 |
| `multiply` | `renderer.rs`（`alto_core::Affine2::concat`、`crates/alto-core/src/lib.rs`） | 字形の行列。JS は同じ項の順で写す |
| `max_scale`、`place`、`GlyphKey`、`GlyphAtlas` | `glyph_atlas.rs` | 帯の選択、アトラスの配置と鍵 |
| `RENDER_VERSION_EXACT` | `crates/alto-core/src/lib.rs` | `m5.rs` の `blend`、`stage`、`raw_node` と同じ分岐 |

前提の PR は二つある。#24 の v3 composite の実装（ホストの Q14 量子化と WGSL の 16384² 除算）と、#26 の「実装順」の (c)（M5 の v3 の中間を `rgba16uint` にし、タイル出力を `div_q_i64(q * 255, 16384)` の `rgba8uint` にする）である。テキストはそれを共有し、別の版を書かない。

## 何を整数にするか

境は **Q8 の頂点**である。そこから先は全部整数で、そこより前は f64 の加減乗除と、正しく丸められる演算だけである。

| 段 | 型 | 一致の根拠 |
| --- | --- | --- |
| フォント単位の座標 | f32 を f64 に広げたもの | `ttf_parser` が Rust の一つのソースで作る。JS は数値を受け取る（下の「JS の入力」） |
| 配置行列、`multiply`、`/ scale` | f64 | 加減乗除だけ。Rust は `mul_add` を書かない限り融合しない。JS も融合しない。IEEE 754 で一意 |
| `max_scale` | f64 | 加減乗と `sqrt`。`sqrt` は IEEE 754 が正しい丸めを要求し、Rust、wasm、JS の `Math.sqrt` で同じ |
| 帯と端数の量子化（`place`） | f64 → i32 / u8 | `round`、`floor`、`ceil` だけ。丸めの規則は下で固定する |
| 頂点の Q8 | f64 → i64 | 下の `q8`。2 の冪の積は正確 |
| 曲線の分割数、分割点、セル積分、バイト | i64 | 整数 |
| 文字色と着色、重ね | Q14 の u32 / i32、`Wide` | 整数 |

禁止するもの。f32 の `acc` と走行和、`hypot`、曲線の分割での `sqrt`、`exp` などの libm、`Math.round` を負の値に使うこと（JS の `Math.round` は負の結びを +∞ 側へ送り、Rust の `round` は 0 から遠い側へ送る）、テキストの v3 での `textureSample`、MSAA、ハードウェアのブレンド。

JS の丸めは `roundAway(x) = Math.sign(x) * Math.round(Math.abs(x))` とする（結果の `-0` は 0 として扱う）。`Math.round` は仕様で「最も近い整数、結びは +∞ 側」で、非負の値では Rust の `round` と一致する。`Math.floor(x + 0.5)` は使わない。`0.49999999999999994 + 0.5` が 1 に丸まるからである。

## 座標の系

ホストは ROI の原点を整数の出力画素で持つ。`render_text` の `roi = [x, y, scale, w, h]` について、`ox = x / scale`、`oy = y / scale` が f64 でちょうど整数で、絶対値が 2^24 以下なら、v3 は**レベル全体の画素座標**で配置と量子化を行い、整数の `ox`、`oy` は窓の位置を決めるときだけ引く。こうすると、アトラスの鍵と Q8 の頂点が ROI に依らない。

この条件は自動ではない。タイルの ROI は `compose_m5`（`m5.rs`）が `x: key.1 as f32 * 256.0 * scale` の f32 で作り、`TileKey` の `tx`、`ty` は i32 である（`crates/alto-core/src/lib.rs`）。`ox = tx * 256` なので、条件が成り立つのは `|tx|, |ty| <= 65536` のときである。その範囲では f32 の積も正確である。今の M5 が ROI を変える二つの関数は、整数の原点を保つ。`input_roi` はガウスぼかしの半径を `ceil` した整数の画素数だけ `Roi::expand` で広げ、`Roi::coarsen` は原点を新しい倍率の格子へ `floor` で揃える。

条件が成り立たない ROI（`|tx|` か `|ty|` が 65536 を超えるタイル）では、今の `render_text` と同じ `(t[4] - x) / scale` を使う。式は同じで決定的だが、継ぎ目の一致は主張せず、parity にも入れない。

v3 の配置行列は次である。`t = multiply(world, g.transform)`、`s = scale`。

```text
m = [t0 / s, t1 / s, t2 / s, t3 / s, t4 / s, t5 / s]     // レベル全体の出力画素
```

## 帯の選び方

`em_px = run.em * (max_scale([w0, w1, w2, w3]) / scale)` は今と同じ式、同じ順である。帯は字形ごとに、次の順で決まる。

1. `m` の六つの値のどれかが有限でなければ、その字形は描かない。三実装で同じ判定である。
2. `em_px` が有限で `em_px < ATLAS_MAX_EM_PX`、かつ `place(&g.outline, m)` が Some なら、**アトラス帯**。`use_atlas` には依らない。ビットマップは `p.raster`、大きさ `p.w × p.h`、置き場所はレベル座標の `p.dst` から `(ox, oy)` を引いたもの。
3. それ以外はすべて**直接帯**。量子化しない `m` をそのまま使い、ROI の窓へ切り抜いてラスタ化する。帯の量子化（`STEPS`、`PHASES`）はしない。

`place`（`glyph_atlas.rs`）が None を返す理由は、`upem` が有限でないか 0 以下、`m` が有限でない、丸めた帯の絶対値が 1e6 を超える、**量子化したあとの**線形部分の行列式が `1e-12` 未満、整数の原点の絶対値が 1e15 を超える、ビットマップの幅か高さが 1 未満か `MAX_GLYPH_PX` を超える、の六つである。v3 は理由を区別しない。どれも直接帯へ送る。行列式の判定は量子化した帯についてのもので、元の `m` が正則でも帯が退化することがある（たとえば `upem = 1000`、`m = [0.012, 0, 0, 0.00001, 0, 0]` の帯は `[48, 0, 0, 0]`）。直接帯は `upem` も行列式も使わないので、細い字形もそのまま描く。元の `m` が本当に退化していれば、頂点が一本の線に潰れ、下のセル積分が 0 の被覆を出す。特別な扱いは要らない。v2 の `fallback.push(g.to_path())` は v3 にない。

`use_atlas` が偽でも、アトラス帯の字形は同じ `p.raster` と同じ大きさでラスタ化し、キャッシュせずにスクラッチへ置く（下の「アトラスとスクラッチ」）。アトラスが満杯で `insert` が None のときも同じである。どちらも、アトラスに入った場合と同じバイトが同じ画素に載る。**キャッシュの状態と描画設定は画素を変えない。** これが v3 の不変条件である。

`place` の丸めは今の Rust の文のままで、JS は次のように写す。`band = roundAway(m[i] * upem * STEPS)`（帯は負になり得て、`-x.5` の結びがある）、`i = Math.floor(o)`、`p = Math.round((o - i) * PHASES)`（非負）、`p >= PHASES` なら `i += 1, p = 0`。上の六つの None の判定、角の min / max、`floor`、`ceil` と ±1 のパディングも同じ順である。`place` は f64 の `x0 as i64` で `dst` を作る。v3 ではこの `o` が上のレベル座標の `t4 / s` である。

アトラスの鍵 `GlyphKey` に `raster: u8` を足す。v1 / v2 は 0、v3 は 1 である。同じ字形の v2 と v3 のビットマップは違うので、同じアトラスに二つの文書が開いていても混ざらない。v2 の鍵の値は変わらず、画素も変わらない。

## 頂点の Q8

```text
q8(v) = roundAway(v * 256)      // Rust は (v * 256.0).round() as i64
```

`v * 256` は 2 の冪の積で正確である。`|v * 256| >= 2^52` の点を持つ字形はラスタ化しない（どの実装でも描かない）。

点は `Move`、`Line`、`Quad` の制御点と終点、`Cubic` の二つの制御点と終点である。すべて、行列を掛けた直後に `q8` を通す。行列の掛け方は今の `rasterize` の `tf` と同じ `m0 * x + m2 * y + m4`、`m1 * x + m3 * y + m5` の順である。アトラス帯は `p.raster`（ビットマップの左上が原点）、直接帯は `m`（レベル全体の画素）を掛ける。この Q8 を `G` と呼ぶ。`G` は窓に依らない。

**字形の原点。** 一つの字形の全部の点について、

```text
A = (floor_div(min G_x, 256), floor_div(min G_y, 256))     // 整数の画素
L = G - 256 * A                                            // 字形の座標。どの成分も 0 以上
```

とする。`floor_div` は負の数でも床である（Rust の `div_euclid`、JS の BigInt では負のときに補正する）。以下の分割とセル積分は、すべてこの**字形の座標** `L` で行う。`A` も `L` も窓に依らない。

**前提の検査。** すべての点で `L_x <= 2^24` かつ `L_y <= 2^24`（字形の広がりが約 65536 画素以下）であること。満たさない字形は描かない。判定は字形だけで決まり、窓に依らないので、ある窓で描かれ別の窓で落ちる、ということは起きない。

**窓。** 窓は、アトラス帯ではビットマップの `[0, p.w) × [0, p.h)`、直接帯では、字形の `bounds` の四隅を `m` で写した箱を外側へ整数に丸め、ROI の `[0, w) × [0, h)` と交わらせた矩形である。交わりが空なら描かない。窓の幅と高さを `ww`、`wh`、窓の左上を字形の画素座標で `(lx, ly)` とする。直接帯では `lx = wx + ox - A_x`（`wx` は ROI の中の窓の左端）、アトラス帯では `lx = -A_x` で、`ly` も同じである。どれも整数の画素なので、窓の境は字形の座標でも 256 の倍数である。

## 曲線の分割

v2 の分割数は `n = ceil(sqrt(10 * |dd|))`（二次）と `ceil(sqrt(30 * |dd|))`（三次）を 1..=64 に切ったもので、`|dd|` は二階差分の長さ（画素）である。v3 はこれを整数で同値に書く。`D` を二階差分の Q8 の長さの二乗（二次は `P0 - 2C + P1`、三次は `P0 - 2A + B` と `A - 2B + P1` の大きいほう）とすると、`|dd| = sqrt(D) / 256` で、

```text
n >= sqrt(K * sqrt(D) / 256)   <=>   65536 * n^4 >= K^2 * D      (K = 10 または 30)
```

である。`n` は 1..=64 で上の不等式を満たす最小の整数、どれも満たさなければ 64 とする。`sqrt` も `hypot` も呼ばない。比較は u64 で行う。

分割点は Bernstein の整数形を一回だけ割る。`i = 1..=n` について、

```text
二次: q = div_q_i64((n-i)^2 * P0 + 2 (n-i) i * C + i^2 * P1, n^2)
三次: q = div_q_i64((n-i)^3 * P0 + 3 (n-i)^2 i * A + 3 (n-i) i^2 * B + i^3 * P1, n^3)
```

を x と y で別々に計算する。点は字形の座標 `L`（0 以上）なので、分子はいつも非負である（「カバレッジ」の最後の段落）。`i = n` は `P1` そのものになる（分子が `n^k * P1` なので余りがない）。分割点を前の分割点から増分で作らない。一点ごとに独立に端点から計算するので、誤差は溜まらない。

線分は、`Move` のたびに開いた輪郭を閉じる線、`Close` の閉じる線、末尾で閉じる線を含め、今の `rasterize` と同じ規則で出す（`cur != start` のときだけ）。比較は Q8 の整数で行う。

## カバレッジ（セル積分）

FreeType の gray ラスタライザと同じ形のセル積分である。浮動小数はない。一画素は Q8 で 256 である。座標は字形の座標 `L`（上の「頂点の Q8」）で、窓の境を

```text
XL = 256 * lx,  XR = 256 * (lx + ww),  YT = 256 * ly,  YB = 256 * (ly + wh)
```

とする。セルは窓の画素 `(cx, cy)`（`0 <= cx < ww`、`0 <= cy < wh`）で、二つの i64 `cover[cx, cy]` と `area[cx, cy]` を持つ。初期値は 0 である。

**線分を行に分ける。** 線分 `(x0, y0) - (x1, y1)` で `y0 == y1` なら捨てる。`y0 > y1` なら端点を入れ替えて `dir = -1`、そうでなければ `dir = +1`。行の境 `Y`（256 の倍数）のうち、`y0 < Y < y1` かつ `YT <= Y <= YB` のものすべてで、

```text
x(Y) = x0 + sign(x1 - x0) * ((|x1 - x0| * (Y - y0) + (y1 - y0) / 2) / (y1 - y0))
```

を作る（非負の `div_q`、`/` は切り捨て）。端点と境の点を y の順に並べ、隣り合う二点を一つの行の小片にする。小片の y の範囲が `[YT, YB]` の外なら捨てる。境の点は元の端点と境だけから計算するので、ある境での値は窓に依らない。窓が違うと、計算する境の集合が違うだけである。

**小片を列に分ける。** 行の小片 `(xa, ya) - (xb, yb)`（`ya < yb`）で、列の境 `X`（256 の倍数）のうち、`min(xa, xb) < X < max(xa, xb)` かつ `XL <= X <= XR` のものすべてで、

```text
y(X) = ya + ((yb - ya) * |X - xa| + |xb - xa| / 2) / |xb - xa|
```

を作る。`|X - xa|` は小片に沿って増え、丸めは非負の数の単調な関数なので、`y(X)` は `ya..=yb` の中で単調である。点を xa から xb の順に並べ、隣り合う二点をセルの小片 `(ua, va) - (ub, vb)` にする。

**セルへ足す。** `dy = vb - va` が 0 の小片は捨てる（足しても 0 である）。

- `ua` と `ub` が両方 `XR` 以上なら捨てる。窓の右の小片は、窓のどの画素にも寄与しない。
- 両方 `XL` 以下なら、`ua = ub = XL` に切る。窓の左の小片は、行の全画素へ `dy` だけ寄与する。切っても結果は同じである（下の式で `fa = fb = 0` の寄与は `512 * dy` で、元の列での寄与と同じだけ前置和に入る）。窓の左で一本の小片にまとまっても、境で分けた複数の小片の `dy` の和と等しい（境の `y` が打ち消し合う）。
- `cx = (ua + ub) div 512 - lx`（床）、`cy = (va + vb) div 512 - ly`。`va < vb` は同じ行にいるので `cy` は一意で `0..wh` である。`cx == ww` は `ua = ub = XR` のときだけで、上で捨てている。
- `fa = ua - 256 (cx + lx)`、`fb = ub - 256 (cx + lx)`（0..=256）。

```text
cover[cx, cy] += dir * dy
area[cx, cy]  += dir * (fa + fb) * dy
```

整数の和は結合的で可換なので、線分と小片を足す順は結果に影響しない。v2 の f32 と違い、積算の順は規定しなくてよい。

**画素の値。** 各行 `cy` を `cx = 0` から右へ走り、`S` を `cover` の前置和（そのセルを含む）とする。

```text
S += cover[cx, cy]
N  = S * 512 - area[cx, cy]          // 1 画素の完全な被覆が 131072
```

`N / 131072` が、その画素の符号付きの被覆である。導出は、セル内の小片の右側の面積が `dy * (256 - (fa + fb) / 2) / 65536` 画素、左のセルの小片がこのセルへ `dy / 256` 画素ずつ寄与する、ということである。和に 131072 を掛けると上の式になる。

**窓の独立性。** 点の Q8（`G`）、字形の原点 `A`、前提の検査、曲線の分割点、行と列の境での値は、どれも字形だけで決まり、窓に依らない。窓が変えるのは、どの境で分けるか、どこで切るかだけで、窓の左の小片は前置和へ同じだけ入り、右の小片は寄与しない。したがって直接帯の窓のバイトは、同じ字形を十分大きな窓でラスタ化した結果の切り出しと全バイト一致する。タイルの継ぎ目で字形が割れても、両側の画素は一枚で描いた場合と同じである（上の「座標の系」の条件のもとで）。

曲線の分割を窓の座標でしてはいけない。`div_q_i64` の丸めは 0 をまたぐ平行移動で変わる。二次の Q8 の点 `P0 = (255, 0)`、`C = (256, 15)`、`P1 = (255, 0)` は `D = 904` で `n = 2` になり、中点の x は `div_q_i64(1022, 4) = 256` である。窓を 1 画素右へずらすと x は `(-1, 0, -1)` で、中点は `div_q_i64(-2, 4) = -1`、戻すと 255 になる。字形の座標 `L` は 0 以上なので、分子はいつも非負で、この問題は起きない。

## バイト

非ゼロ則は今と同じく `|winding|` を 1 で切る近似である。

```text
byte = (min(|N|, 131072) * 255 + 65536) >> 17
```

結びは `|N| * 255 = 131072 k + 65536` のときで、255 と 2^16 は互いに素なので `|N| = 65536`（半分の被覆）だけである。このとき `byte = 128` で、0 から遠い側である。`N = 0` は 0、`|N| >= 131072` は 255 である。

v2 の `rasterizer_matches_exact_area` の「3/4 被覆の列が 191」は、v3 でも `N = 98304` から `(98304 * 255 + 65536) >> 17 = 191` になる。v3 の単体テストも同じ値を見る。

## アトラスとスクラッチ

アトラスは今の `R8Unorm`（`ATLAS_SIDE` 四方）一枚のままにする。v3 用の `R8Uint` は作らない。#24 の理由（VRAM、アップロード、キャッシュが二つになる）と同じである。直接帯の字形と、キャッシュしないアトラス帯の字形（`use_atlas` が偽、または `insert` が None）は、`R8Unorm` のスクラッチテクスチャに置く。

- **切り抜き。** スクラッチへ載せるのは、ビットマップと ROI の `[0, w) × [0, h)` の交わりだけである。アトラス帯の 96×96 までのビットマップも、狭い ROI では切り抜く。インスタンスの `dst` と `src` は切り抜いた矩形を指す。直接帯の窓は、もともと ROI との交わりである。したがって一枚のビットマップは、いつも ROI の大きさに収まる。
- **容量で区切る。** スクラッチの大きさは ROI の `bucket` の大きさ（`alloc` と同じ）で、棚詰めは `GlyphAtlas::new(w, h)` を割り当てにだけ使う。次の切り抜きが入らないとき、または未送出のインスタンスが `GLYPH_Q14_CHUNK` に達したとき、そこまでのインスタンスを一つのディスパッチとして送り、**新しい**スクラッチテクスチャで続ける。区切りはインスタンスの順を変えないので、結果は変わらない（下の「着色と重ね」のチャンクと同じ理由）。
- **同じテクスチャを使い回さない。** `queue.write_texture` は、あとで送るコマンドバッファより先にまとめて実行される。同じエンコーダの中でスクラッチを書き直すと、前のディスパッチが新しい内容を読む。区切りごとに別のテクスチャを作り、`retired` に入れて捨てる。各テクスチャは `check_allocation` で `Class::Vector` の予算に数える。予算が足りなければ、今の `render_text` と同じくエラーを返す（画素を変えて続けない）。
- 書き込みは今と同じ `queue.write_texture` のバイト列である。GPU の変換を通らない。
- 読みは `textureLoad` と `unorm_byte` だけである。`textureSample`、サンプラ、ハードウェアのフィルタは使わない。
- #24 の必須検査は `Rgba8Unorm` だけを見ている。`R8Unorm` は別の形式なので、検査を足す。256 通りのバイトを `R8Unorm` の 256×1 のテクスチャに入れ、`textureLoad` + `unorm_byte` で全値が戻ることを、`Gpu::atlas_read` と `measureAtlasRead` の同じ実行で確かめ、`atlas-read.json` / `parity.json` の `atlasRead` に `format: "R8Unorm"` の行を足す。どれかの値が戻らなければ、`gpu_shader_matches_cpu_reference`（Metal、lavapipe）とブラウザのパリティ（SwiftShader）が失敗する。不一致が出たら、そのときに `R8Uint` を検討する。

v2 の `glyph-atlas.wgsl` は、同じアトラスを f32 の `.r` で読み続ける。テクスチャの内容は鍵で分かれている（上の `GlyphKey` の `raster`）。

## 着色と重ね

v3 はノードの中で字形を、インスタンスの順（`runs` の順、ラン内の `glyphs` の順。今の `instances` と同じ）に、透明な Q14 のアキュムレータへ #24 の normal で重ねる。整数のテクスチャはハードウェアでブレンドできないので、重ねは計算シェーダの中で行う。

**文字色。** `style.color` は f32 のストレート RGBA である。ホスト（Rust と JS）は各チャネルを Q14 の `c_r, c_g, c_b, c_a`（0..=16384）にする。新しい関数 `text_color_q14`（Rust は `renderer.rs`、JS は `textColorQ14`）で、定義は実数の `floor(clamp(c, 0, 1) * 16384 + 1/2)` である（有限でない値は 0）。実装は、f32 を **先に f64 へ広げて**から `clamp`、16384 倍、0.5 の加算、`floor` を f64 で行う。`color_adjust_q14`（同ファイル）は積と和を f32 で行うので、使わない。f32 の和は丸めが入り、f64 の JS と一致しない（例：`c = (0.5 - 2^-25) / 16384` は f32 の和が 1 に丸まって 1、実数では 0）。f64 では、`x = c * 16384` は 2 の冪の積で正確で、`x + 0.5` も正確である（`x >= 2^-29` なら `x` の 24 ビットの仮数と 0.5 が 53 ビットに収まり、`x < 2^-29` なら和は 1 未満で床は 0）。したがって Rust と JS は同じ整数を得る。JS は `textGlyphs` から f32 の値をそのまま f64 で受け取るので、`Math.fround` は要らない。WGSL に f32 の色はない。

**一つの字形の画素。** `b` はカバレッジのバイト、`k = byte_q14(b)`。#26 のモード 4 と同じく、係数を Q28 の一つの整数にまとめる。

```text
F     = k * c_a                          // 0..=2^28
s_a   = div_q(F, 16384)                  // = div28(16384 * F)
s_rgb = div28(c_rgb * F)                 // プレマルチプライ Q14、一回丸め
```

字形は「不透明な色 `(c_rgb, 16384)` に、カバレッジ `k` と不透明度 `c_a` のマスクを掛けたもの」で、#26 のモード 4 の `div28(v * F)` そのものである。`c_rgb <= 16384` なので、`div28` の単調性から `s_rgb <= s_a` である。

**重ね。** #24 の normal で、全チャネル同じ式である。

```text
acc' = div_q(s * 16384 + acc * (16384 - s_a), 16384)
```

`acc` の初期値は 0 である。`b = 0` なら `s = 0` で `acc' = acc` なので、飛ばしても同じ結果になる。`acc_a <= 16384` から `acc'_a <= 16384`、`acc_rgb <= acc_a` から `acc'_rgb <= acc'_a` である（#24 の normal の証明と同じ）。`min(rgb, a)` は入れない。

結果のノードは M5 の v3 の中間（`rgba16uint`、プレマルチプライ Q14）である。親への合成とタイルのバイトは (c) と #24 が決める。透明な背面への normal は Q14 のコピーなので、parity の単独のテキストのタイルは `div_q_i64(acc * 255, 16384)` である。

**WGSL。** 新しいシェーダ `crates/alto-gpu/shaders/glyph-q14.wgsl`（計算、8×8）。`glyph-atlas.wgsl` は変えない。

```text
struct GlyphQ14 { dst: vec4<i32>, src: vec2<u32>, tex: u32, pad: u32, color: vec4<u32> }
// dst = (ROI 画素の x, y, w, h)、src = テクスチャ内の左上、tex = 0 アトラス / 1 スクラッチ、color = Q14 のストレート RGBA
@group(0) @binding(0) var<uniform> c: vec4<u32>;            // (first, count, roi_w, roi_h)
@group(0) @binding(1) var<storage, read> glyphs: array<GlyphQ14>;
@group(0) @binding(2) var atlas: texture_2d<f32>;           // R8Unorm
@group(0) @binding(3) var scratch: texture_2d<f32>;         // R8Unorm
@group(0) @binding(4) var prev: texture_2d<u32>;            // rgba16uint、前のチャンク
@group(0) @binding(5) var out: texture_storage_2d<rgba16uint, write>;
```

画素ごとに、`first == 0` なら `acc = 0`、そうでなければ `acc = textureLoad(prev, p, 0)`。`i = first .. first + count` の順に、`l = p - dst.xy` が `0 <= l < dst.zw` のとき、`tex` で選んだテクスチャの `src + l` を `textureLoad` し、`.r` を `unorm_byte` でバイトにして、上の式を適用する。最後に `textureStore(out, p, acc)`。`div_q`、`unorm_byte`、`byte_q14`、`Wide`、`wide_mul_u32`、`wide_add` は `effects.wgsl` の文を写し、写した文が一致することをテスト（`glyph_q14_uses_effects_helpers`、新規）が見る。`div28` は #26 と同じく `wide_add` で `Wide(1u << 27u, 0u)` を足し、`(lo >> 28u) | (hi << 4u)` を商にする。

インスタンスは `GLYPH_Q14_CHUNK = 1024` 個ずつのディスパッチに分け、`prev` と `out` を入れ替えて順に流す。中間は Q14 の u16 で損失がないので、チャンクの切り方は結果を変えない。インスタンスが 0 個なら、ノードは透明（全 0）である。

**参照。** Rust は `text_q14`（`cpu_composite.rs`、テストのみ、新規）、JS は `renderTextV3`（`cpu-composite.js`、新規）が、同じインスタンスの列から同じ画素ごとのループを行う。カバレッジのバイトは、Rust は本番の `rasterize_q8`、JS は独立に書いた `rasterizeQ8` が作る。

## JS の入力

JS は整形を持たない。rustybuzz、`ttf_parser`、改行、禁則を JS に書き直すのは、この経路の決定性の論点ではない（native と wasm が同じ Rust で作る）。JS が受け取るのは整形の結果である。

wasm にテスト用の要求 `textGlyphs`（`#[wasm_bindgen(js_name=textGlyphsForTesting)]`、`packages/alto-sdk/src/worker.ts` の `readTileLevel` と同じ並びに case を足す）を足す。返すのは、テキストノードごとの `runs` の配列で、各ランの `em` と、各字形の `outline`（`key`、`upem`、`bounds`、`commands`）、`transform`、`color` である。数値は serde_json の f64 で、最短の往復表現なので、JS の `JSON.parse` で同じビットに戻る。`world` と ROI は JS が文書のスナップショットとタイルから作り、`multiply` を `Affine2::concat` と同じ項の順で書く。

その先、帯の選択、`place`、`q8`、分割、セル積分、バイト、着色、重ね、出力のバイトはすべて JS が独立に計算する。整数はすべて BigInt で、除算は `divQBig` である。

## ヒンティングと AA のリスク

**ヒンティング。** main にはヒンティングがない。`OutlineCache::outline` は `ttf_parser` の `outline_glyph` で生の輪郭を取り、rustybuzz の送りもヒンティングしないフォント単位である。v3 はこれを規則にする。

- TrueType のバイトコード、オートヒンタ、FreeType、CoreText、DirectWrite、canvas の `fillText` を、タイルに入る画素の経路で使わない。ヒンティングの結果はインタプリタの版と実装で変わり、OS ごとに違う。ピアは文書のバイトから同じ画素を作れなくなる。
- ステムダークニング、ガンマやコントラストの補正、LCD のサブピクセル AA を入れない。どれもカバレッジのバイトの後に表示の都合で入る非線形の変換で、タイルの内容ハッシュに入るべきではない。入れるなら表示（`present.wgsl`）の側で、タイルの外である。
- フォントは文書のアセット（`FontSet`）だけである。システムフォントへのフォールバックはない。欠けた字形は `OutlineCache::outline` の固定のプレースホルダ（1000 単位の箱）で、どこでも同じである。

**AA。** v3 のアンチエイリアスは上のセル積分（面積被覆）だけで、ハードウェアの MSAA を使わない。理由は「前提」の 1 と「決定」の 3 である。整数の頂点を渡しても、サンプル位置（Vulkan の `standardSampleLocations` は必須でない）、ラスタライザの固定小数の精度（`subPixelPrecisionBits` は 4 以上なら何でもよい）、上と左の規則の実装、resolve の平均と丸めは、Metal、lavapipe、SwiftShader で揃う保証がない。

**残るリスク。** 次の二つは、整数化の外にある。

1. **整形と配置の f64。** 加減乗除だけで、Rust の一つのソースである。Rust がこれを融合しない（`mul_add` を書かない限り）ことと、wasm の f64 が IEEE 754 であることに依る。3 バックエンドが同じ v3 のハッシュに一致することが、この部分の native と wasm の一致の検査を兼ねる（Metal と lavapipe は native、SwiftShader は wasm）。
2. **`ttf_parser` の CFF の解釈。** チャーストリングは f32 で解釈され、`ttf_parser` の版で結果が変わり得る。版を上げれば v3 の画素が変わる。`Cargo.lock` の版は、v3 のロック済みハッシュの一部とみなす（「未解決の問題」の 3）。

## 溢れの上限

上限は解析で、標本ではない。前提は二つで、どちらもラスタ化の前に三実装が同じ判定で検査する。字形の座標の点は `0 <= L <= 2^24`（窓に依らない）。一つの字形の線分の数 `L` は、`placed` の `CHUNK`（4000 コマンド）と分割数の上限 64 から `L <= 4000 * 64 = 256_000` である（`Close`、`Move`、末尾の閉じる線は、それぞれ一つのコマンドが一本だけ出す）。

| 項 | 上限 | 入る型 |
| --- | --- | --- |
| 二階差分の成分 | 点が 0..=2^24 なので `|P0 - 2C + P1| <= 2 * 2^24 = 2^25` | i64 |
| `D = ddx^2 + ddy^2` | `2 * 2^50 = 2^51` | u64 |
| `K^2 * D`（三次、`K = 30`） | `900 * 2^51 = 2_026_619_832_316_723_200` | u64、i64（`2^63 - 1 = 9_223_372_036_854_775_807` 未満） |
| `65536 * n^4` | `2^16 * 64^4 = 2^40` | u64 |
| Bernstein の分子 | 非負。係数の和が `n^3 <= 2^18` なので `2^18 * 2^24 = 2^42` | i64 |
| 行の分割 `|dx| * (Y - y0)` | 境は端点の間にあるので `2^24 * 2^24 = 2^48` | i64 |
| 列の分割 `(yb - ya) * |X - xa|` | `256 * 2^24 = 2^32` | i64 |
| 小片の `(fa + fb) * dy` | `512 * 256 = 2^17` | i32 |
| セルの `area` | `L * 2^17 = 33_554_432_000` | i64。i32 には入らない |
| 行の前置和 `S` | `L * 256 = 65_536_000` | i64 |
| `N = S * 512 - area` | `2 * 33_554_432_000 = 67_108_864_000` | i64 |
| バイトの `131072 * 255 + 65536` | `33_488_896` | u32 |
| `F = k * c_a` | `2^28` | u32、i32 |
| `c_rgb * F + 2^27` | `2^42 + 2^27` | `Wide`、i64 |
| 重ねの `s * 16384 + acc * (16384 - s_a) + 8192` | `2^28 + 8192` | i32 |

セルの `area` の上限は、一本の線分が一つの行の中では x について単調で、各セルに小片を高々一つしか落とさないことに依る（窓の左の部分は一つの小片にまとめる）。`S` は、一本の線分が一つの行へ足す `dy` の和が 256 以下であることに依る。重ねの上限は `s <= s_a` から `16384 * (s_a + 16384 - s_a) = 2^28` である。

ラスタ化は CPU だけなので、`Wide` が要るのは着色の一項だけである。Rust は i64、JS は BigInt である。

## 溢れの検査

テスト名は `text_q8_raster_does_not_overflow` で、`glyph_atlas.rs` のテストに置く（新規）。`color_adjust_q14_mix_does_not_overflow` と同じく、上の各項を `checked_mul` / `checked_add` で計算し、`None` か表の上限の超過で失敗する。

- 字形の座標の端点 `{0, 1, 255, 256, 257, 2^24 - 256, 2^24 - 1, 2^24}` の直積で、二次と三次の二階差分、`K^2 * D`、Bernstein の分子を `n` の 1..=64 の全数で見る。分割数の整数の判定が、`n` の全数と `D` の端点で、f64 で `(K * sqrt(D) / 256).sqrt().ceil()` を計算した値と一致することも見る（端点は f64 で正確な範囲に取る）。
- 行と列の分割の積を、同じ端点の直積で見る。
- `L = 256_000` 本の線分を一つのセルに重ねる最悪の場合（同じ縦線を往復させる）を構成し、`area`、`S`、`N` が上限以下であること。
- バイトの式を `N` の `{0, 1, 65535, 65536, 65537, 131071, 131072, 131073, 67_108_864_000}` とその符号反転で見る。65536 は 128、131072 以上は 255。
- 着色と重ねは `cpu_composite.rs` の `glyph_q14_does_not_overflow`（新規）。`b` の 256 通りすべて × `c_a ∈ E` × `c_rgb ∈ E` × `acc` の `rgb <= a` の `E` の組（55 組）で、`out_a <= 16384` と `out_rgb <= out_a` を表明する。`out_a > 16384` か `out_rgb > out_a` になったら失敗する。`E` は #24 の `{0, 1, 4095, 4096, 4097, 8191, 8192, 8193, 16383, 16384}` である。

単体テストはほかに四つ足す。

- `rasterizer_q8_matches_exact_area`。v2 の `rasterizer_matches_exact_area` と同じ正方形と円で、3/4 被覆の列が 191、内側が 255、円のインクが πr² の 1% 以内。
- `rasterizer_q8_window_crop`。一つの字形を大きな窓で描いた結果と、それを 2×2 に割った四つの窓で描いた結果の切り出しが全バイト一致する。窓の境が字形の中を通る場合、曲線の分割点が境の上に乗る場合、窓の左端が字形の点の 1 画素右にある場合（「カバレッジ」の最後の段落の `P0 = (255, 0)`、`C = (256, 15)`、`P1 = (255, 0)`）、字形の広がりがちょうど `2^24` の場合（どの窓でも描かれる）と `2^24 + 1` の場合（どの窓でも描かれない）を含む。
- `atlas_state_does_not_change_v3_pixels`。`GlyphAtlas::new` を小さくして満杯にし、追い出しと None を起こしても、`use_atlas` を偽にしても、ノードの Q14 が通常のアトラスの場合と全バイト一致する。
- `glyph_key_separates_raster_versions`。同じ字形と行列で `raster` が 0 と 1 の鍵が別のエントリになる。

## 一致の期待

一致は全バイト一致である。バックエンドは #24 と同じ三つである。Metal は native ジョブ（`Apple Paravirtual device`、実機は未検証）、lavapipe は gpu-parity ジョブ、SwiftShader は wasm ジョブ（アダプタの architecture に `swiftshader`）。ブラウザは `tests/fixtures/render-v3-empty.alto` を開いて v3 の文書にし、そこへテキストを足す。

一致を期待する理由は一つである。カバレッジのバイトは Rust の一つのソースの整数で、GPU が受け取る前に決まる。GPU は `unorm_byte` のあと整数の加減乗と非負の除算だけを行い、上の表で溢れがないことを示した。残る f64 はホストの加減乗除と `sqrt`、`floor`、`ceil`、規則を固定した `round` で、IEEE 754 で一意である。

| ケース | 中身 | WGSL = Rust = JS | Metal | lavapipe | SwiftShader | 扱い |
| --- | --- | --- | --- | --- | --- | --- |
| `text` を v3 で | 既存 xfail と同じ "Ag"、72px、`AltoTestLatin.ttf`、黒、`[1, 0, 0, 1, 24, 80]` | 一致 | 一致 | 一致 | 一致 | enforce。直接帯 |
| 小さい文字 | "Ag" を 12px、原点 `(24.3, 80.6)` で端数の位相を踏む | 一致 | 一致 | 一致 | 一致 | enforce。アトラス帯 |
| 縦組 | `AltoTestJP.otf`（CFF の三次）で `vertical_rl`、"あ。" と縦中横、16px と 40px | 一致 | 一致 | 一致 | 一致 | enforce。両帯 |
| 半透明の重なり | 色のアルファ 0.5、`letter_spacing` −0.4 em で隣の字形のインクを重ねる（`style.scale` は送りと字形を同じ倍率で縮めるので、重なりを作らない） | 一致 | 一致 | 一致 | 一致 | enforce。参照は、二つの字形がともに `b = 255` の画素が 1 つ以上あることを確かめ、なければ失敗する。その画素では `F = 2^27`、`s_a = 8192`、重ねて 12288 になる。`s_a = 8192` は全被覆の画素だけで、縁の画素ではない |
| 回転 | 0.3 rad 回したテキスト、12px と 72px | 一致 | 一致 | 一致 | 一致 | enforce |
| アトラスを切る | 小さい文字のケースを `setGlyphAtlas(false)` で | 一致（アトラスありと同じバイト） | 一致 | 一致 | 一致 | enforce |
| タイルの継ぎ目 | 72px の字形を `(250, 250)` に置き、タイル (0,0)、(1,0)、(0,1)、(1,1) | 一致 | 一致 | 一致 | 一致 | enforce。Rust は `rasterizer_q8_window_crop` でも見る |
| テキストにマスクや効果 | マスク、ぼかし、調整レイヤー | 期待しない | — | — | — | 各経路の v3 が入るまで measured-not-enforced |
| 原点の条件を外れる ROI | `|tx|` か `|ty|` が 65536 を超えるタイル | 決定的だが継ぎ目は主張しない | — | — | — | parity に入れない |
| v2 の `text` | `createDocument` のまま | — | — | — | — | xfail のまま |
| v2 の `text` の計測 | 同じ "Ag" を v2 で、3 バックエンドの差の画素数 | — | 計測 | 計測 | 計測 | measured-not-enforced |

Rust の参照は、`TextRun` を `RContent::Text` としてシーンに直接置ける（`plain_scene_gaps_are_explicit` が `RContent::Text(Arc::new(Vec::new()))` を置くのと同じ組み方）。字形は `alto-engine` の `text_entry`（`crates/alto-engine/src/text.rs`）と同じく `glyph_runs` で作る。JS は上の `textGlyphs` を使う。

テキストの新しいケースはレベル 0 だけにする。これはこの文書の制限で、parity 全体の性質ではない（`parityCases` にはレベル 1 の `level1-centers` があり、`readTileLevel` で読む）。レベルが違うと `em_px` が変わり、帯が切り替わる。レベル 1 と負のレベルのテキストのケースは、レベル 0 が固定されてから足す（「未解決の問題」の 10）。

ケースの名前は `text-v3-` で始める（`text-v3-ag`、`text-v3-small`、`text-v3-vertical`、`text-v3-overlap`、`text-v3-rotated`、`text-v3-atlas-off`、`text-v3-seam`）。

## enforce と measure

enforce するのは、構成上一致し、3 バックエンドの CI で全バイト一致したケースだけである。上の表で enforce と書いたものは、実装 PR で一致を確かめてから通常のケースとして入れる。期待どおりに一致しなかったら、measure に落とさない。設計に戻る（#24 のアトラス読み取りと同じ扱い）。`R8Unorm` のアトラス読み取りは、入れた時点から必須である。

measure するのは、v2 の `text` と、テキストに v3 がまだない経路（マスク、効果）を重ねたケースである。`measureRow` と `formatMeasure` で `measure: true, enforced: false` の行にし、失敗にしない。v2 の計測は「前提」の 1 から 4 のどれが効いているかを分けるため、`setGlyphAtlas(false)` の行と 12px の行も持つ。

## v1 / v2 を動かさない

- `glyph-atlas.wgsl`、`vector.wgsl`、`vector-paint.wgsl`、`rasterize`、`accumulate`、`place` の文は一文字も変えない。v3 は `rasterize_q8`（新規）と `glyph-q14.wgsl`（新規）を使う。`place` は v3 でもそのまま呼ぶ（入力の `m` がレベル座標になるだけである）。
- `GlyphKey` の `raster` は v1 / v2 で 0 である。鍵の型が変わるだけで、v2 のビットマップの中身と置き場所の規則は変わらない。
- ホストの分岐は `self.scene.render_version >= alto_core::RENDER_VERSION_EXACT` である。`render_text` は v2 の経路として残し、v3 は `raw_node` の `RContent::Text` から新しい `text_q14`（`m5.rs`）を呼ぶ。
- v2 の「アトラスが満杯ならベクター経路」と `setGlyphAtlas` の効果は v2 に残す。v2 はこの既知の非決定性を持ったまま凍結する。既存文書の対処は `render.upgrade` である。
- `tests/parity/render-v1.txt` と `tests/parity/cpu-reference.txt` の既存行は一文字も変えない。v3 の行を足すだけである。ただし今の `existing_v1_and_v2_hashes_are_unchanged`（`cpu_composite.rs`）は、`cpu-reference-v2-before-alpha.txt` より後の行に `-alpha-measure ` を含むことを要求し、`render-v1.txt` 全体が `render-v1-before-alpha.txt` と等しいことを要求する。`text-v3-` の行はそのままでは落ちる。実装順の 6 で、後ろに足せる行を「`-alpha-measure ` を含む、または名前が `text-v3-` で始まる」に広げる。前置の一致と v1 の全体の一致の検査は残す。v2 のテキストにはロック済みのハッシュがない（xfail）が、出荷済みの v2 の文書の画素は、文を変えないことで守る。
- `plain_scene` と `gap` は変えない。テキストは v3 でも合成パスに乗らない。
- ゲートは今のまま使う。Rust は `allow_unstable_render_v3`（`crates/alto-engine/src/unstable_render.rs`）で、デバッグビルド（`cfg!(debug_assertions)`）、コンパイル時の許可（このクレートのテストかフィーチャ `unstable-render-v3`）、実行時の要求の三つがすべて要る。リリースの成果物はフィーチャがあっても拒否する。JS は `renderVersionUpgrade`（`packages/alto-sdk/src/render-version.ts`）で、`{ unstableRenderV3: true }` を渡しても、ビルドの許可 `buildEnabled`（既定は出荷定数 `UNSTABLE_RENDER_V3` で false）がなければ `rejected` になる。オプションだけでは v3 にならない。
- alto-text は変えない。整形、配置、字形の取り出しは全バージョンで共通である。
- シェーダが変わるのでリリース WASM のバイナリのハッシュとサイズは動く。PR #29（main の `ffdc1bf`）以降、`wasm/SHA256SUMS` と `verification/build.json` は各 PR では更新しない。`RELEASING.md` のとおり、タグの前に main の `wasm-hashes-main` から一度だけ更新し、main ではずれは報告だけである。描画のハッシュとは独立である。

## 実装順

前提は #26 の「実装順」の (b)（v3 composite）と (c)（M5 の Q14 中間）である。(a) の measure-only は PR #28（main の `feb1d67`）で入った。テキストはそれに依らない。テキストはマスク (d) と独立で、(c) のあとならどちらが先でもよい。各項目は main から切った一つの PR である。

1. **ラスタ化の核。** `glyph_atlas.rs` に `q8`、分割数の判定、`flatten_q8`、`rasterize_q8`（窓とオフセットを受ける）を足す。単体テスト `rasterizer_q8_matches_exact_area`、`rasterizer_q8_window_crop`、`text_q8_raster_does_not_overflow`。どこからも呼ばないので、画素は変わらない。
2. **アトラスの鍵と `R8Unorm` の読み取り検査。** `GlyphKey` に `raster` を足し、`glyph_key_separates_raster_versions`。`Gpu::atlas_read` と `measureAtlasRead` に `R8Unorm` の 256 値を足し、必須にする。v2 の画素は変わらない。
3. **Rust の参照。** `cpu_composite.rs` に `text_q14` と `glyph_q14_does_not_overflow`。帯の選択、レベル座標の `m`、アトラス帯と直接帯、着色と重ねを、本番の `rasterize_q8` の上に書く。
4. **WGSL とホスト。** `glyph-q14.wgsl`、`GlyphQ14` のインスタンス、スクラッチの `R8Unorm`、チャンクのディスパッチ、`m5.rs` の `text_q14` と分岐。`glyph_q14_uses_effects_helpers` と `atlas_state_does_not_change_v3_pixels`。native の 2 バックエンドで、上の enforce のケースを Rust の参照と比べる。
5. **JS の参照。** wasm の `textGlyphsForTesting`、worker の `textGlyphs`、`cpu-composite.js` の `rasterizeQ8` と `renderTextV3`（BigInt、`divQBig`、`roundAway`）。
6. **固定。** enforce のケースを 3 バックエンドで流し、一致を記録してから v3 のハッシュとして `cpu-reference.txt` に `text-v3-` の行を足す。同じ PR で `existing_v1_and_v2_hashes_are_unchanged` の追記の規則を広げる（上の「v1 / v2 を動かさない」）。ハッシュの行が要るのは、各ジョブの CPU 参照との比較だけでは native と wasm の整形の一致が見えないからである。Metal と lavapipe は native の整形を、SwiftShader と JS は wasm の整形を使う。
7. **measure の行。** v2 の `text` の計測と、マスクや効果を重ねたケース。

## 未解決の問題

1. **アトラス帯の量子化の画質。** アトラス帯は今と同じく、行列の四つの係数を 1/4 px/em の帯に、原点を 1/4 px の位相に量子化する。決定性には関係しないが、直接帯との境（`em_px = 24`）で字形が跳ぶ。跳びに一律の上限はない。位相の誤差は軸ごとに 1/8 px 以下だが、係数の誤差は係数ごとに `1 / (8 upem)` px／フォント単位で、点のフォント単位の座標 `(x, y)` に比例する。一つの軸で `(|x| + |y|) / (8 upem) + 1/8` px まで足し合わさり、`x = upem` の点は係数 a だけで 1/8 px 近く、位相とで 1/4 px 近く動く。字形の大きさで変わる。境で跳ばないように帯をやめ、全字形を直接帯にするか（アトラスの再利用を捨てる）は決めていない。
2. **直接帯の CPU の費用。** 72px 以上の大きな字形は、タイルごと、レベルごとに CPU でラスタ化し直す。タイルのキャッシュが内容ハッシュで再利用するが、ズーム中の初回の描画は遅くなり得る。窓をまたいだ字形のキャッシュ（窓の独立性があるので、字形ごとの大きなビットマップを切り出してよい）を足すかは、計測してから決める。
3. **`ttf_parser` と rustybuzz の版。** CFF の f32 の解釈と整形の結果は、依存の版で変わり得る。版を上げると v3 のハッシュが変わる。版の更新を「描画バージョンを上げる変更」として扱うか、字形の Q8 の輪郭を文書に焼くか（マスクのように、ピアが式ではなくバイトを受け取る）は決めていない。
4. **ヒンティングを将来入れるか。** 小さい文字の画質のために、整数で決定的なヒンティング（たとえば y 方向だけの整数スナップ）を v4 で入れる余地はある。v3 には入れない。
5. **可変フォント。** main にない。入れるなら、F2Dot14 の差分の補間を整数で行い、Q8 の前の f32 を作らない規則が要る。
6. **巨大な字形。** 広がりが約 65536 画素を超える字形（字形の座標で `2^24` を超える点を持つもの）は v3 で描かない。判定は字形だけで決まり、窓に依らない。極端なズームで実際に現れるかは調べていない。現れるなら、点の上限を i128 で広げるか、直線の切り詰めを先に行う。
7. **ブラウザの入力の重複。** `textGlyphs` はテストのための wasm の出口である。JS の参照が整形を持たないので、整形の native と wasm の一致は 3 バックエンドのハッシュの一致だけが間接に見る。整形の結果（字形 ID と行列）のハッシュを native と wasm で直接比べる検査を足すかは決めていない。
8. **チャンクと binning。** `GLYPH_Q14_CHUNK` の 1024 と、画素ごとに全インスタンスを見るループは、文字の多いタイルで遅い。16×16 のビンに分けても順を保てば結果は変わらないが、いつ入れるかは計測してから決める。
9. **古いメモの指し先。** [正確な CPU 参照](2026-10-27-exact-cpu-references.md) の「text」節は、この文書で置き換えた。あの節からこの文書への指し先は、実装 PR で足す。この変更は文書を一つ足すだけで、既存の文書は触らない。
10. **レベル 0 以外のケース。** テキストの新しい parity はレベル 0 だけである。帯の切り替わり（`em_px` が 24 をまたぐ）をレベル 1 と負のレベルで固定するケースを、いつ足すかは決めていない。
