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
| 字形の取り出しと整形 | — | 編集（`bake_text` が焼き込みを作る）。v3 の描画にはない | — |
| 焼き込みの検証、node 変換の Q16 と合成、帯、`place_q` | — | 本番 | 参照（`cpu-composite.js`、新規） |
| Q8 の点、分割、カバレッジ、バイト | — | 本番 `glyph_atlas.rs`（参照も兼ねる） | 参照（`cpu-composite.js`、新規） |
| アトラスの読み、着色、重ね | 本番（新しい `glyph-q14.wgsl`） | 参照（`cpu_composite.rs`、テストのみ、新規） | 参照（同上） |

ラスタ化の Rust は参照と本番が同じ関数である。参照として独立なのは JS である。WGSL がラスタ化を持たないことは、「各経路を三度実装する」の例外ではなく、その経路が GPU に存在しないということである。

## 範囲

| 対象 | 対象外 |
| --- | --- |
| 焼き込み（下の「焼き込み」）の形式、検証、字形の同一性 | 焼き込みを作る整形そのもの（rustybuzz、改行、禁則）の式。編集であり、結果のバイトが文書に入る |
| テキストノードのカバレッジ（全サイズ、横組・縦組・縦中横・回転・水平比率） | テキストノードを親へ載せる合成。M5 の v3（#26 の「実装順」の (c)）と #24 のブレンドの範囲 |
| アトラスの鍵、追い出し、満杯時の扱い | 可変フォント、カラーフォント（COLR、sbix）。main にない（`set_variation` を呼ぶ箇所はない） |
| 着色（文字色の Q14）と、ノード内の字形の重ね、ノードの Q14 中間（`rgba16uint`）への書き込み | テキストの選択範囲やキャレットの描画。UI であり、タイルに入らない |

## main と #24・#26 から使うもの

新しい丸めは作らない。結びは 0 から遠い側へ行く。

| 名前 | 場所 | ここでの役割 |
| --- | --- | --- |
| `div_q_i64` / `divQBig` | `cpu_composite.rs` / `cpu-composite.js` | 行列の合成、点の Q8、曲線の評価、帯、位相の除算（符号あり）。カバレッジの分割の除算（非負） |
| `round_half_away`、`quantize_q16` | `crates/alto-gpu/src/sample.rs` | v1 / v2 用。v3 のテキストは**使わない**（「数の境」の反例） |
| `div_q`、`Wide`、`wide_mul_u32`、`wide_add` | `crates/alto-gpu/shaders/effects.wgsl` | 着色と source-over。`glyph-q14.wgsl` は別シェーダなので文を写す |
| `unorm_byte` | `effects.wgsl`。Rust の計測側は `cpu_composite.rs` の `UNORM_BYTE_WGSL` | アトラスの `.r` をバイトに戻す |
| `byte_q14` / `byteQ14` | `effects.wgsl`、`cpu_composite.rs`、`cpu-composite.js` | カバレッジのバイトを Q14 にする |
| `div28`（#26） | #24 の 16384² 除算。`(n + 2^27) >> 28`、`n >= 0` | 着色の一回丸め |
| `Gpu::atlas_read`、`atlas_read_shader`、`measureAtlasRead` | `cpu_composite.rs`、`tests/e2e/parity-tests.js` | アトラス読み取りの必須検査。ここで `R8Unorm` を足す |
| `measureRow`、`formatMeasure` | `parity-tests.js` | measured-not-enforced の行 |
| `GlyphKey`、`GlyphAtlas`、`ATLAS_MAX_EM_PX`、`STEPS`、`PHASES`、`MAX_GLYPH_PX` | `glyph_atlas.rs` | アトラスの鍵と割り当て、帯の定数。v2 の `place` と `max_scale` は v3 で呼ばない（下の `place_q`） |
| `ContentHash::of`（blake3） | `crates/alto-core/src/lib.rs` | フォントと焼き込みの同一性。フォントのアセットの `hash` は `put_blob`（`crates/alto-engine/src/lib.rs`）がこれで作る |
| `RENDER_VERSION_EXACT` | `crates/alto-core/src/lib.rs` | `m5.rs` の `blend`、`stage`、`raw_node` と同じ分岐 |

前提の PR は二つある。#24 の v3 composite の実装（ホストの Q14 量子化と WGSL の 16384² 除算）と、#26 の「実装順」の (c)（M5 の v3 の中間を `rgba16uint` にし、タイル出力を `div_q_i64(q * 255, 16384)` の `rgba8uint` にする）である。テキストはそれを共有し、別の版を書かない。

## 数の境

v3 の契約（Linus の裁定、reviews-31b）：浮動小数は字形を作るとき（焼き込み）だけ許す。焼き込みの結果は整数で保存し、描画はその整数と文書の値のビット列だけを入力にする。描画には、比較・丸め・正確な積を含め、浮動小数の演算を一つも置かない。

- **描画（v3 の範囲）。** 入力は文書のバイトだけである。テキストノードの焼き込み（整数の輪郭、整数の配置、Q14 の色）、node 変換、タイルの鍵。文書が f64 で持つ node 変換は浮動小数として扱わない。各値の IEEE-754 のビット列（u64）を整数として分解し、Q16 の i64 にする（下の「浮動小数のビットの整数分解」）。`sample.rs` の `quantize_q16` / `round_half_away` は v1 / v2 用に凍結したまま、v3 では使わない。あれは `scaled + 0.5` を f64 で足すので 0.5 の直下で誤る（`v = 7.629394531249999e-06` は `scaled = 0.49999999999999994` で、`+ 0.5` が 1.0 に丸まって 1 を返す。半分は 0 から遠い丸めの正しい値も JS の `Math.round` も 0。手元の f64 で再現した）。合成は整数で行う（下の「座標の系」）。`alto-text` の `layout`、`glyph_runs`、`OutlineCache`、`ttf_parser`、rustybuzz、unicode の各クレートは、v3 の描画から一度も呼ばない。
- **編集（焼き込み）。** 焼き込みは、テキストの編集操作が作るバイトである。作る過程は今の `layout`（f64）と `ttf_parser`（f32）を使ってよい（裁定どおり）。浮動小数から整数への丸めは Rust の `f64::round`（実数の値を半分は 0 から遠い側へ丸める。`+ 0.5` を足さない）だけを使う。ピアは式ではなく、内容ハッシュを持つバイトを受け取る。同じ編集を二つのピアが別々に焼いても、文書に入るのは操作に載った一つの焼き込みだけである。

したがって、整形と字形の取り出しの f32 / f64 は v3 の描画に残らない。v3 の描画の決定性は、焼き込みのバイトから始まる。焼き込みを作る側を整数の整形器に替えても、描画の式は変わらない（「未解決の問題」の 3）。

禁止するもの。v3 の描画での f32 / f64 の演算（比較、丸め、加減乗除、`sqrt`、`hypot`、libm）、`OutlineCache` と `TextRun` の読み、焼き込みが無いときの描画時の焼き直し、`textureSample`、MSAA、ハードウェアのブレンド。JS の参照は BigInt だけで、文書の数はビット列（16 進の文字列）で受け取る。

| 段 | 型 | 根拠 |
| --- | --- | --- |
| 焼き込みの輪郭 | フォント単位の Q6、i32 | 文書のバイト |
| 焼き込みの配置 | 係数 Q24（node 画素／フォント単位）i32、平行移動 Q30（node 画素）i64 | 文書のバイト |
| 文字色 | Q14 の u16 | 文書のバイト |
| node 変換 | 文書の f64 のビット列（u64）→ Q16 の i64（整数の分解） | 下の「浮動小数のビットの整数分解」 |
| 行列の合成、帯、位相、点の Q8 | i128 の積、`div_q_i64` | 整数 |
| 曲線の分割数、分割点、セル積分、バイト | i64 / u64 | 整数 |
| 着色と重ね | Q14 の u32 / i32、`Wide` | 整数 |

## 焼き込み

v3 の文書のテキストノードは、`NodeKind::Text`（`crates/alto-doc/src/lib.rs`）に `bake: Option<ContentHash>` を持つ。`SetText` も同じ欄を持ち、内容と焼き込みを**一つの操作**で置く。v1 / v2 の文書では常に None で、直列化では欄ごと省く。v1 / v2 の文書の JSON と内容ハッシュは変わらない。`DocumentMeta::validate` は `render_version` が 3 未満の文書に `bake` があれば拒否する。

焼き込みの本体はブロブで、ハッシュは `ContentHash::of(bytes)`、置き場はラスタのタイルと同じブロブの保管である。形式は次のとおり（すべてリトルエンディアン）。

```text
magic        "ALTB"            4 bytes
version      u16 = 1
baker        u16               焼いたビルドの BAKER_VERSION。描画は読まない（診断用）
font_count   u32
  font[i]    hash [u8; 32]     フォントのアセットの ContentHash（blake3）
             face_index u32
outline_count u32
  outline[j] font u32          font の添字。0xFFFFFFFF はプレースホルダ
             glyph u16         字形 ID（プレースホルダは 0）
             upem u16          1..=16384
             cmd_count u32     <= 4000（CHUNK）
             cmd[k]            tag u8（0 Move、1 Line、2 Quad、3 Cubic、4 Close）+ i32 の座標を 2、2、4、6、0 個
glyph_count  u32
  glyph[g]   outline u32       outline の添字
             m [i32; 4]        Q24。font units -> node 画素の [a, b, c, d]（列優先、Affine2 と同じ）
             t [i64; 2]        Q30。node 画素
             color [u16; 4]    Q14 のストレート RGBA
```

字形の並びは描く順（今の `layout.glyphs` の順）である。輪郭は `(font, glyph)` で重複を除き、最初に出た順に並べる。

**描画時の検証。** 描画は焼き込みを読む前に、magic、version、各数の上限（`upem` が 1..=16384、`cmd_count <= 4000`、輪郭の座標が `|v| <= 2^22`、`t` が `|t| <= 2^60`、添字が範囲内、ブロブの末尾がちょうど尽きる）を検査する。一つでも外れたら、そのノードは透明に描き、診断に `TEXT_BAKE_INVALID` を出す。焼き込みが None の v3 のテキストノードも透明で、`TEXT_UNBAKED` を出す。どちらもどのピアでも同じ画素である。描画時に焼き直すことはしない。焼き直しは `render.upgrade`（`upgrade_render_version` / `upgradeRenderVersion`）と `SetText` の編集だけが行い、どちらも取り消せる一つの編集である。

**焼き方（編集側、参考）。** エンジンの新しい `bake_text`（`crates/alto-engine/src/text.rs`）が、`layout` の結果から作る。

- フォントは `font_set` と同じく `(hash, face_index)` で読む。輪郭は `ttf_parser::Face::outline_glyph` を自前の `OutlineBuilder` で受ける。`alto-text` の `OutlineCache` は使わない。あれは `(AssetId, u16)` を鍵にし、`key()` も同じ値を `DefaultHasher` で混ぜるだけで、フォントのバイトも `face_index` も含まない。
- 座標は `q6(v) = (v as f64 * 64.0).round()`、係数は `(v * 2^24).round()`、平行移動は `(v * 2^30).round()`（いずれも `f64::round`。`sample.rs` の `round_half_away` は使わない）、色は下の `text_color_q14` である。輪郭の上限（`CHUNK`）と配置の上限（`LIMIT`）は `placed`（`outline.rs`）と同じ判定を写す。
- 焼き込みの鍵（同じ内容を二度焼かないためのキャッシュ）は `text_entry` の鍵に各フォントの `face_index` を足したものにする。今の `text_entry` はアセットの `hash` だけを足しており、同じバイトで `face_index` だけが変わると古いレイアウトを返す。v3 の焼き込みはその鍵を使わない。

## 字形の同一性

v3 の描画の入力は焼き込みのバイトだけなので、字形の同一性はそこで閉じる。

- **フォント。** 焼き込みの `font[i]` が `(hash, face_index)` を持つ。`SetAsset` は同じ `AssetId` の記録を置き換えられる（`Op::SetAsset`、`alto-doc`）が、v3 の描画はアセットの表もフォントのバイトも読まないので、置き換えは焼き込みを変えない限り画素を変えない。v3 の文書で、焼き込みが参照するフォントのアセットを `SetAsset` が置き換えるとき、エンジンはその文書の該当するテキストノードの `SetText`（焼き直し）を**同じ取引**に入れる。焼き直さない古いクライアントの操作が来ても、全ピアは同じ古い焼き込みで描く。
- **フォールバックの順。** スタイルの `fonts`（`TextStyle.fonts`、スパンの上書きを含む）の順が、焼いたときの順である。焼き込みには、実際に使われたフォントだけが `font[i]` として入る。
- **輪郭と鍵。** アトラスの v3 の鍵は `GlyphKey` に足す `raster: u8`（v1 / v2 は 0、v3 は 1）と `outline_hash: [u8; 32]` である。`outline_hash` は `ContentHash::of` を、その輪郭の記録のバイト（`upem` から最後の `cmd` まで、`font` と `glyph` を除く）に掛けたものである。同じ形の輪郭は同じエントリを共有し、違う形は違うエントリになる。v3 では `outline: u64` は 0 にする。v1 / v2 の `GlyphKey` の値は `raster = 0`、`outline_hash = [0; 32]` で、今と同じ分かれ方をする。
- **parity のフォント。** `tests/fonts/AltoTestLatin.ttf` は blake3 `cecfaa71790a6d0aa8744ddfdb3cb9fd6fbac9e2f8e0db476827710df1e0dbc3`（SHA-256 `47829824d7f0ddb6e79d20daa796651b80642643adf358dd73bcb5666f93e5f2`）、TrueType、face 0。`tests/fonts/AltoTestJP.otf` は blake3 `5e2213b3dfdcc9736b3bd951f51ec58935a3bde47bc4f9521838f042c0db8981`（SHA-256 `55d67c5374912f2cb5296e14ceae8c1d29f22df1ddccb0cf023b0be0f2ce71c9`）、CFF、face 0。parity は読み込んだアセットの `hash` がこの blake3 と等しいことを表明し、違えば失敗する。

## 依存の版

v3 の描画は `ttf_parser`、rustybuzz、unicode の各クレートを呼ばないので、版を上げても**既存の v3 文書の画素は変わらない**。版が変えるのは、新しい編集が作る焼き込みのバイトだけである。それは新しい内容ハッシュの新しい文書の状態で、ピアへはバイトとして届く。

それでも焼き込みの再現性のために、版を固定する。

- `crates/alto-text/Cargo.toml` の `rustybuzz = "0.20.1"` を `"=0.20.1"` にする。
- 新しい検査 `text_bake_dependencies_are_pinned`（`alto-engine` のテスト）が `Cargo.lock` を読み、`rustybuzz 0.20.1`、`ttf-parser 0.25.1`、`unicode-bidi 0.3.18`、`unicode-linebreak 0.1.5`、`unicode-segmentation 1.13.3`、`unicode-script 0.5.8`、`unicode-ccc 0.4.0`、`unicode-properties 0.1.4` であることを確かめる。
- 新しいフィクスチャ `tests/parity/text-bake.txt` に、parity の各テキストを焼いた焼き込みの blake3 を置く。native と wasm の両方で焼き、どちらもこの値に一致しなければ失敗する。依存の版を上げてこの値が変わる PR は、`BAKER_VERSION` を上げ、フィクスチャを更新し、その理由を書く。既存の v3 の描画のハッシュ（`cpu-reference.txt` の `text-v3-` の行）は、焼き込みのフィクスチャから描くので、その PR でも変わらない。

## 座標の系

v3 のテキストは、M5 の f32 の `Roi` と f64 の `world` を使わない。ホストは同じ再帰（`compose_m5` → `node` → `raw_node`）の中で、整数の二つを並べて運ぶ。

- **整数の ROI。** `RoiQ { level: i32, ox: i64, oy: i64, w: u32, h: u32 }`。タイルでは `TileKey` の i32 から `ox = tx * 256`、`oy = ty * 256` で、浮動小数を通らない。`input_roi` の `Roi::expand` は整数の画素数だけ広げるので、`ox -= r`、`w += 2r` と同じ整数で運ぶ。`Roi::coarsen` は倍率を変える。ガウスぼかしの `stack`（`m5.rs`、`desired.coarsen(radius / 64.0)`）がテキストを含む部分木をこの ROI で描くので、v3 は同じ写像を整数で持つ。新しいレベル `L'` は `max(L, k)` で、`k` は `2^k >= minimum`、すなわち `64 * 2^k >= rf` を満たす最小の整数である。`rf` は v2 の `coarsen(radius / 64.0)` が使うのと同じ f32 の半径（シーンの `REffectKind::GaussianBlur { radius: f32 }`）で、`k` はその**正確な**ビット列から決める。Q8 に丸めた半径からは決めない（文書の定義域の中の例で、半径 32.001 は f32 で `32.00099945…` なので k = 0 だが、Q8 の `R = 8192` からは k = -1 になり、ずれる）。この `k` は v2 の f32 の `log2` の結果とすべての半径で一致するわけではない。v2 の `log2` の戻り値は f32 に丸められるので、glibc では `nextafter(1, inf)` のレベル −6 と `nextafter(2, inf)` のレベル −6、−5 で、v2 の倍率が v3 より一段細かい（ぼかしのメモの「粗い倍率」の v2 との関係）。v3 は v2 の値ではなく `k` の式に従う。定義と関数は [v3 整数ぼかし](2026-10-28-v3-integer-blur.md)（PR #32）の「粗い倍率」のもので、ぼかしの段と同じ `alto_core::blur::blur_coarsen_k`（JS は `blurCoarsenK`）を使う。正規化数は `m = frac | 2^23`、`e = E - 150`、非正規化数は `m = frac`、`e = -149` として `rf = m * 2^e` とし、`k = clog2(m) + e - 6` を整数のビット長から求める（`rf = 0` なら coarsen しない）。浮動小数の比較も `log2` も使わない。ぼかしの v3 はレベル `-6..=12` だけを受けるので、この `L'` を使うのもその範囲である（ぼかしのメモの「レベルの検査」）。原点と大きさは `coarsen` の式の整数形で、文書画素の `X = ox * 2^L` から `ox' = floor_div(X, 2^L') - 1`、`w' = ceil_div((ox + w) * 2^L - ox' * 2^L', 2^L') + 1`（y も同じ）。ぼかし自体は v2 のままなので、その出力は enforce しない（「一致の期待」のテキストに効果を重ねた行）。テキストノードの Q14 は決定的である。
- **整数の変換。** `WorldQ { m: [i64; 4], t: [i64; 2] }`。node 画素からレベルの出力画素への写像で、係数も平行移動も Q16 である。根は恒等 `[65536, 0, 0, 65536]`、`[0, 0]` で、レベル `L` の倍率 `2^L` は最後に掛ける（下）。各 node で、文書の f64 の node 変換 `n` の各値のビット列を `q_from_f64_bits(bits, 16)`（下）で Q16 にし（係数が `|q| >= 2^31` か有限でなければ、その部分木のテキストは描かずに `TEXT_TRANSFORM_RANGE` を出す）、親と整数で合成する。

```text
child.m[a] = div_q_i64(p.m[0] * n.m[0] + p.m[2] * n.m[1], 2^16)    // a, b, c, d も Affine2::concat と同じ項
child.t[x] = div_q_i64(p.m[0] * n.t[0] + p.m[2] * n.t[1] + p.t[0] * 2^16, 2^16)
```

積は i128 で計算する。合成の結果の係数が `|m| >= 2^31` か、平行移動が `|t| >= 2^47`（2^31 画素）なら、同じく描かない。レベルの倍率は、テキストノードで一度だけ `m`、`t` の各成分を `L >= 0` なら `div_q_i64(v, 2^L)`、`L < 0` なら `v * 2^-L`（`L >= -16`）にする。ここまで浮動小数の算術はない。合成の丸めは段ごとに一回で、深さは `MAX_TREE_DEPTH` が抑える。

**字形の写像。** 字形 `g` の焼き込みの配置 `P = (g.m, g.t)`（Q24 と Q30）と `W`（Q16）から、フォント単位をレベルの画素へ送る `C` を作る。

```text
C.m[a] = div_q_i64(W.m[0] * P.m[0] + W.m[2] * P.m[1], 2^16)        // Q24。レベル画素／フォント単位
C.t[x] = div_q_i64(W.m[0] * P.t[0] + W.m[2] * P.t[1] + W.t[0] * 2^30, 2^16)     // Q30。レベル画素
```

`b`、`c`、`d` と `y` も `Affine2::concat` と同じ項である。積は i128。`|C.m| >= 2^31` か `|C.t| >= 2^60` の字形は描かない。

## 浮動小数のビットの整数分解

描画が文書の f64 を読むのは node 変換（とぼかしの半径の f32。こちらは下の `q_from_f64_bits` ではなく、上の「座標の系」の `blur_coarsen_k` で分解する）だけで、どれもビット列を整数として分解する。浮動小数の演算はない。

```text
q_from_f64_bits(bits: u64, f: u32) -> Option<i64>       // 値 v を Q f にし、半分は 0 から遠い側へ丸める
s = bits >> 63;  e = (bits >> 52) & 0x7ff;  frac = bits & (2^52 - 1)
e == 0x7ff            -> None                              // 無限大と NaN
e == 0                -> m = frac,          x = -1074      // 非正規数と ±0
それ以外              -> m = frac | 2^52,   x = e - 1075   // |v| = m * 2^x
k = x + f
k >= 0: k > 9 なら None。そうでなければ q = m << k        // m < 2^53 なので q < 2^62
k < 0:  r = -k。r >= 54 なら q = 0（m < 2^53 <= 2^(r-1)）。そうでなければ q = (m + 2^(r-1)) >> r
s == 1 なら q = -q（-0 は 0）
```

`k < 0` の式は `floor(|v| * 2^f + 1/2)` を整数で正確に計算したもので、符号を最後に付けるので半分は 0 から遠い側へ行く。f32 は同じ形で、`e` が 8 ビット、`frac` が 23 ビット、`x = e - 150`（非正規数は `-149`）、`r >= 25` で 0 である。JS はビット列を BigInt で受け取り、同じ整数の手順を書く。値は `textBake` の要求がビット列の 16 進で返す（下の「JS の入力」）。

Rust と JS の Q16 変換の規範式を以下に固定する。Rust の `to_bits()` と JS へ渡す 16 進文字列はビットの取り出しだけで、数値演算をしない。`None` / `null` は同じ範囲外を表す。変換後、係数は `|q| < 2^31`、平行移動は `|q| < 2^47` を別に検査する。上の一般形も、使う `f` は 16 に限る。前の版はぼかしの半径の Q8 の `rq` のために `f = 8` と f32 版を使っていたが、`coarsen` の `k` は Q8 から決めなくなったので（「座標の系」）、テキストの描画は f32 版も `f = 8` も使わない。上の f32 の形は、`blur_coarsen_k` と同じビット分解の記録として残す。

```rust
fn text_q16_from_bits(bits: u64) -> Option<i64> {
    let e = ((bits >> 52) & 0x7ff) as i32;
    if e == 0x7ff { return None; }
    let frac = bits & ((1u64 << 52) - 1);
    let (m, x) = if e == 0 { (frac, -1074) }
                 else { (frac | (1u64 << 52), e - 1075) };
    let k = x + 16;
    let q = if k >= 0 {
        if k > 9 { return None; }
        m << (k as u32)
    } else {
        let r = (-k) as u32;
        if r >= 54 { 0 } else { (m + (1u64 << (r - 1))) >> r }
    } as i64;
    Some(if bits >> 63 != 0 { -q } else { q })
}
```

```javascript
function textQ16FromBits(hex) {
  const bits = BigInt("0x" + hex);
  const e = (bits >> 52n) & 0x7ffn;
  if (e === 0x7ffn) return null;
  const frac = bits & ((1n << 52n) - 1n);
  const m = e === 0n ? frac : frac | (1n << 52n);
  const k = (e === 0n ? -1074n : e - 1075n) + 16n;
  let q;
  if (k >= 0n) {
    if (k > 9n) return null;
    q = m << k;
  } else {
    const r = -k;
    q = r >= 54n ? 0n : (m + (1n << (r - 1n))) >> r;
  }
  return bits >> 63n !== 0n ? -q : q;
}
```

再現値は正の f64 のビット列 `3edfffffffffffff`、`3ee0000000000000`、`3ee0000000000001`（Q16 の 0.5 の直下・ちょうど・直上）である。期待値は順に `0, 1, 1`。符号ビットを立てた `bedfffffffffffff`、`bee0000000000000`、`bee0000000000001` は `0, -1, -1`。Q16 の 1.5 の前後 `3ef7ffffffffffff`、`3ef8000000000000`、`3ef8000000000001` は `1, 2, 2`、符号反転は `-1, -2, -2`。Node.js で旧 Rust と同じ floor/ceil 式、旧 JS の `sign * Math.round(abs(scaled))`、新しい整数式を実行し、最初の正負の値だけが旧 Rust で `±1`、旧 JS と新式で `0` になることを再現した。これらは十進の再パースに頼らず、同じビット列で Rust/JS を比較する固定ベクトルである。

検査 `q_from_float_bits_rounds_half_away`（新規）は、`±2^-17`（Q16 でちょうど ±0.5、±1 になる）、その直下 `±7.629394531249999e-06`（0）、直上（±1）、`±1.5 * 2^-16`（±2）とその前後、最小の非正規数、±0、±∞、NaN、`k = 9` と `k = 10` の境を、Rust と JS の両方で有理数の正しい丸めと比べる。凍結した `quantize_q16` がこの直下の値で 1 を返すことも表明し、v3 で使わない理由として残す。

**丸めの規則。** v3 のテキストの丸めは一つで、`div_q_i64` で**その量の有理数の値全体**を一度だけ丸める。変位を丸めてから整数を足す形はない。量ごとの基準の系は次のとおりで、系の間の移しかえは整数の画素（Q8 で 256 の倍数）の平行移動だけであり、丸めではない。`WorldQ` と `C`（レベル画素）、`place_q` の帯と位置（レベル画素）、`bx`、`by`（字形の原点の画素）、直接帯の `G`（レベル画素）、Bernstein の点と行と列の境（字形の座標 `L`、非負）。移しかえは `bx - 256 x0`、`L = G - 256 A`、窓の `lx`、`ly` の三つである。

## 帯の選び方

帯は字形ごとに、`C` と輪郭の `upem` だけで決まる。

```text
frob = C.m[0]^2 + C.m[1]^2 + C.m[2]^2 + C.m[3]^2            // u128
atlas の条件: frob * upem^2 < 2 * 24^2 * 2^48
```

これは「Frobenius ノルムを √2 で割った em の画素数が `ATLAS_MAX_EM_PX` 未満」である。回転と一様な倍率では v2 の `max_scale` と同じ値で、異方的な行列では最大と最小の特異値の間に来る。v3 は v2 の `max_scale`（f64 の `sqrt`）を呼ばず、この整数の判定を帯の定義にする。`run.em` も使わない。

1. 条件を満たし、下の `place_q` が Some なら**アトラス帯**。`use_atlas` には依らない。
2. それ以外はすべて**直接帯**。`C` をそのまま使い、ROI の窓へ切り抜いてラスタ化する。帯の量子化（`STEPS`、`PHASES`）はしない。

**`place_q`（新規、`glyph_atlas.rs`）。** v2 の `place` は f64 で、`o.floor()` のあとの非負の端数を `round` するので、負の位置の結びが 0 から遠い側へ行かない（`o = -0.125` で `i = -1`、`p = round(3.5) = 4` が繰り上がって `(0, 0)` になる。0 から遠い丸めなら `-0.25`）。v3 は `place` を呼ばず、整数の `place_q` を使う。`place` の文は変えない。

```text
band[i]  = div_q_i64(C.m[i] * upem * 4, 2^24)                       // 1/4 px/em の帯。符号あり、0 から遠い丸め
q[k]     = div_q_i64(C.t[k], 2^28)                                  // 1/4 px の位置。符号あり、0 から遠い丸め
origin[k] = q[k].div_euclid(4)
phase[k]  = q[k].rem_euclid(4)                                      // 0..=3
```

`o = -0.125` は `q = div_q_i64(-2^27, 2^28) = -1` で、`origin = -1`、`phase = 3`（`-0.25`）である。`o = -0.375` は `-1.5` から `q = -2`、`origin = -1`、`phase = 2`。`o = 0.125` は `q = 1`、`(0, 1)`。帯が退化（`band[0] * band[3] - band[1] * band[2] == 0`、整数で正確）なら None。

ビットマップの大きさは、焼き込みの輪郭の制御点の境界（Q6、整数）の四隅を帯で写して決める。点 `(X, Y)`（Q6）の、原点からの Q8 は

```text
bx = div_q_i64(band[0] * X + band[2] * Y + 64 * phase[0] * upem, upem)
by = div_q_i64(band[1] * X + band[3] * Y + 64 * phase[1] * upem, upem)
```

である（`band / (4 upem)` 画素／フォント単位 × `X / 64` を Q8 にすると `band * X / upem`）。四隅の最小と最大から、`x0 = floor_div(min bx, 256) - 1`、`x1 = ceil_div(max bx, 256) + 1`（y も同じ）、`w = x1 - x0`、`h = y1 - y0`。`w` か `h` が 1 未満か `MAX_GLYPH_PX` を超えれば None。`dst = origin + (x0, y0)`（レベル画素）、ROI の中では `dst - (ox, oy)`。

`place_q` が None なら直接帯である。v2 の `fallback.push(g.to_path())` は v3 にない。

`use_atlas` が偽でも、アトラス帯の字形は同じ点と同じ大きさでラスタ化し、キャッシュせずにスクラッチへ置く（下の「アトラスとスクラッチ」）。アトラスが満杯で `insert` が None のときも同じである。どちらも、アトラスに入った場合と同じバイトが同じ画素に載る。**キャッシュの状態と描画設定は画素を変えない。** これが v3 の不変条件である。

## 頂点の Q8

点は `Move`、`Line`、`Quad` の制御点と終点、`Cubic` の二つの制御点と終点で、焼き込みの Q6 の整数 `(X, Y)` である。Q8 の点 `G` は次のとおり。

- アトラス帯。ビットマップの左上を原点として、上の `bx - 256 x0`、`by - 256 y0`。
- 直接帯。レベル全体の画素で `G_x = div_q_i64(C.m[0] * X + C.m[2] * Y + C.t[0], 2^22)`、`G_y` も同じ（Q30 から Q8）。積は i128。

どちらも浮動小数を通らず、窓に依らない。

**字形の原点。** 一つの字形の全部の点について、

```text
A = (floor_div(min G_x, 256), floor_div(min G_y, 256))     // 整数の画素
L = G - 256 * A                                            // 字形の座標。どの成分も 0 以上
```

とする。`floor_div` は負の数でも床である（Rust の `div_euclid`、JS の BigInt では負のときに補正する）。以下の分割とセル積分は、すべてこの**字形の座標** `L` で行う。`A` も `L` も窓に依らない。

**前提の検査。** すべての点で `L_x <= 2^24` かつ `L_y <= 2^24`（字形の広がりが約 65536 画素以下）であること。満たさない字形は描かない。判定は字形だけで決まり、窓に依らないので、ある窓で描かれ別の窓で落ちる、ということは起きない。

**窓。** 窓は、アトラス帯ではビットマップの `[0, w) × [0, h)`、直接帯では、輪郭の境界の四隅を `C` で写した Q8 の箱を外側へ画素に丸め（`floor_div` と `ceil_div`）、ROI のレベル画素の `[ox, ox + w) × [oy, oy + h)` と交わらせた矩形である。交わりが空なら描かない。窓の幅と高さを `ww`、`wh`、窓の左上を字形の画素座標で `(lx, ly)` とする。直接帯では `lx = (窓の左端のレベル画素) - A_x`、アトラス帯では `lx = -A_x` で、`ly` も同じである。どれも整数の画素なので、窓の境は字形の座標でも 256 の倍数である。

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
x(Y) = div_q_i64(x0 * (y1 - y0) + (x1 - x0) * (Y - y0), y1 - y0)
```

を作る。丸め前の有理数を `x_exact` とすると分子は `x_exact * (y1 - y0)` で、`x_exact` は 0 以上の端点の凸結合なので非負であり、丸めは座標そのものの「半分は上」になる。変位を丸めて足す形（`x0 + sign * round(|dx| t)`）は使わない。x が減る辺で座標の丸めにならないからである（`(257, 0) - (256, 512)` の `Y = 256` は実数で 256.5、変位の形は 256、この式は `div_q_i64(257 * 512 - 256, 512) = 257`）。端点と境の点を y の順に並べ、隣り合う二点を一つの行の小片にする。小片の y の範囲が `[YT, YB]` の外なら捨てる。境の点は元の端点と境だけから計算するので、ある境での値は窓に依らない。窓が違うと、計算する境の集合が違うだけである。

**小片を列に分ける。** 行の小片 `(xa, ya) - (xb, yb)`（`ya < yb`）で、列の境 `X`（256 の倍数）のうち、`min(xa, xb) < X < max(xa, xb)` かつ `XL <= X <= XR` のものすべてで、

```text
y(X) = div_q_i64(ya * |xb - xa| + (yb - ya) * |X - xa|, |xb - xa|)
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

**窓の独立性。** 点の Q8（`G`）、字形の原点 `A`、前提の検査、曲線の分割点、行と列の境での値は、どれも字形だけで決まり、窓に依らない。窓が変えるのは、どの境で分けるか、どこで切るかだけで、窓の左の小片は前置和へ同じだけ入り、右の小片は寄与しない。したがって直接帯の窓のバイトは、同じ字形を十分大きな窓でラスタ化した結果の切り出しと全バイト一致する。タイルの継ぎ目で字形が割れても、両側の画素は一枚で描いた場合と同じである。

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

**文字色。** 描画が読むのは焼き込みの `color`（Q14 のストレート RGBA、0..=16384）だけである。WGSL にも Rust と JS の描画にも f32 の色はない。焼くとき（編集側）は、`style.color` の f32 を新しい `text_color_q14`（`crates/alto-engine/src/text.rs`）で Q14 にする。定義は実数の `floor(clamp(c, 0, 1) * 16384 + 1/2)` で、有限でない値は 0 である。実装は有限性を検査し、f32 を f64 へ広げて clamp した後、`(c * 16384.0).round()` を使う（焼き込み側だけ）。2 の冪の積は正確で、`f64::round` が半分を 0 から遠い側へ丸める。`color_adjust_q14`（`renderer.rs`）は積と和を f32 で行い、`c = (0.5 - 2^-25) / 16384` で 1 を返す（実数では 0）ので使わない。

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

JS の参照は、v3 の描画と同じく焼き込みのバイトから始まる。整形も字形の取り出しも持たない。それは描画の範囲ではないので（上の「数の境」）、JS の参照の独立性は描画の全体に及ぶ。

wasm にテスト用の要求 `textBake`（`#[wasm_bindgen(js_name=textBakeForTesting)]`、`packages/alto-sdk/src/worker.ts` の `readTileLevel` と同じ並びに case を足す）を足す。引数はノードの ID で、返すのはそのノードの焼き込みのブロブのバイト（`Uint8Array`）と、その `ContentHash` である。JS は blake3 を計算せず、受け取ったハッシュが文書のスナップショットの `bake` と等しいことだけを確かめる。

その先、焼き込みの検証と読み、node 変換の Q16（`textBake` が根からそのノードまでの各 node 変換を f64 のビット列の 16 進で返し、JS は `q_from_f64_bits` を BigInt で写す。文書のスナップショットの JSON の数は読まない）、`WorldQ` と `C` の合成、帯、`place_q`、Q8、分割、セル積分、バイト、着色、重ね、出力のバイトはすべて JS が独立に計算する。整数はすべて BigInt で、除算は `divQBig` である。

## ヒンティングと AA のリスク

**ヒンティング。** main にはヒンティングがない。`OutlineCache::outline` は `ttf_parser` の `outline_glyph` で生の輪郭を取り、rustybuzz の送りもヒンティングしないフォント単位である。v3 はこれを規則にする。

- TrueType のバイトコード、オートヒンタ、FreeType、CoreText、DirectWrite、canvas の `fillText` を、タイルに入る画素の経路で使わない。ヒンティングの結果はインタプリタの版と実装で変わり、OS ごとに違う。ピアは文書のバイトから同じ画素を作れなくなる。
- ステムダークニング、ガンマやコントラストの補正、LCD のサブピクセル AA を入れない。どれもカバレッジのバイトの後に表示の都合で入る非線形の変換で、タイルの内容ハッシュに入るべきではない。入れるなら表示（`present.wgsl`）の側で、タイルの外である。
- フォントは文書のアセット（`FontSet`）だけである。システムフォントへのフォールバックはない。字形の代わりは、今の `layout` で二種類あり、v3 はどちらも焼き込みにそのまま入れる。新しい振る舞いは足さない。
  - **使えるフォントが一つもない**（スタイルの `fonts` のどれも読めない）とき。`Itemizer` の `faces` に何もなく、`layout.rs` は書記素ごとに `GlyphRef { font: None, glyph: 0 }` を置く。輪郭は `OutlineCache::outline` の固定のプレースホルダ（1000 単位の箱と穴）で、焼き込みでは `font = 0xFFFFFFFF`、`upem = 1000` の輪郭になる。
  - **フォントはあるが、どれもその書記素を持たない**とき。`Itemizer::font_for` は `fonts` の順で最初に読めるフォントを返し、rustybuzz はそのフォントで整形して字形 0（`.notdef`）を出す。`layout` は `missing` に記録するだけで、字形はそのまま残る。描かれるのはそのフォントの `.notdef` の輪郭で、箱とは限らず、空のこともある。焼き込みにはその輪郭がそのまま入る。
  どちらも焼き込みのバイトで決まるので、ピアによって変わらない。

**AA。** v3 のアンチエイリアスは上のセル積分（面積被覆）だけで、ハードウェアの MSAA を使わない。理由は「前提」の 1 と「決定」の 3 である。整数の頂点を渡しても、サンプル位置（Vulkan の `standardSampleLocations` は必須でない）、ラスタライザの固定小数の精度（`subPixelPrecisionBits` は 4 以上なら何でもよい）、上と左の規則の実装、resolve の平均と丸めは、Metal、lavapipe、SwiftShader で揃う保証がない。

**整数化の外に残るもの。** v3 の描画にはない。整形（rustybuzz）、字形の取り出し（`ttf_parser` の CFF の f32 を含む）、`layout` の f64 は、焼き込みを作る編集の中だけにある。版や算術の違いは新しい焼き込みのバイトを変え得るが、既存の文書の画素は変えない（上の「依存の版」）。

## 溢れの上限

上限は解析で、標本ではない。変換と配置の上限は、上の「座標の系」と「焼き込み」の検査が先に押さえる。`WorldQ` の係数 `|m| < 2^31`、平行移動 `|t| < 2^47`、焼き込みの係数は i32、平行移動 `|t| <= 2^60`、`C` の係数 `|C.m| < 2^31`、平行移動 `|C.t| < 2^60`、輪郭の座標 `|X|, |Y| <= 2^22`、`upem <= 16384 = 2^14` である。ラスタ化の前提は二つで、どちらもラスタ化の前に三実装が同じ判定で検査する。字形の座標の点は `0 <= L <= 2^24`（窓に依らない）。一つの字形の線分の数 `L` は、`placed` の `CHUNK`（4000 コマンド）と分割数の上限 64 から `L <= 4000 * 64 = 256_000` である（`Close`、`Move`、末尾の閉じる線は、それぞれ一つのコマンドが一本だけ出す）。

| 項 | 上限 | 入る型 |
| --- | --- | --- |
| `WorldQ` と `C` の合成の積の和 | `2 * 2^31 * 2^31 = 2^63`（係数）、`2 * 2^31 * 2^60 = 2^92`（平行移動） | i128 |
| `frob * upem^2` | `4 * 2^62 * 2^28 = 2^92` | u128 |
| `place_q` の `C.m * upem * 4` | `2^31 * 2^14 * 4 = 2^47` | i64 |
| `place_q` の `band * X`（アトラス帯は `band` が約 136 以下） | `2 * 2^8 * 2^22 = 2^31` | i64 |
| 直接帯の `C.m * X + C.m * Y + C.t` | `2 * 2^31 * 2^22 + 2^60 < 2^61` | i128（i64 にも入る） |
| 二階差分の成分 | 点が 0..=2^24 なので `|P0 - 2C + P1| <= 2 * 2^24 = 2^25` | i64 |
| `D = ddx^2 + ddy^2` | `2 * 2^50 = 2^51` | u64 |
| `K^2 * D`（三次、`K = 30`） | `900 * 2^51 = 2_026_619_832_316_723_200` | u64、i64（`2^63 - 1 = 9_223_372_036_854_775_807` 未満） |
| `65536 * n^4` | `2^16 * 64^4 = 2^40` | u64 |
| Bernstein の分子 | 非負。係数の和が `n^3 <= 2^18` なので `2^18 * 2^24 = 2^42` | i64 |
| 行の分割の分子全体 | `x0 * dy` と `dx * (Y-y0)` の絶対値は各 `<= 2^48`。和の絶対値は `<= 2^49`（凸結合では `<= 2^48`） | i64 |
| 列の分割の分子全体 | `ya * |dx| <= 2^48`、`dy * |X-xa| <= 2^32`、和は `< 2^49` | i64 |
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
- 行と列の分割の分子全体と積を、同じ端点の直積で見る。`intersection_q8_rounds_coordinate` を Rust/JS に足す。行の `(257,0)-(256,512)` と `(256,0)-(257,512)` は `Y=256` でともに 257、端点を逆順にしても同じ交点で `dir` だけ反転する。`Y=256` で `(257,0)-(256,511)` は 256、終点 y を 513 にすると 257（tie の直下・直上）。列の `(255,257)-(257,258)` は `X=256` で 258、左右を反転しても 258。期待値は有理数の座標全体の half-away から求め、窓の切り出し検査にもこれらを含める。
- `L = 256_000` 本の線分を一つのセルに重ねる最悪の場合（同じ縦線を往復させる）を構成し、`area`、`S`、`N` が上限以下であること。
- バイトの式を `N` の `{0, 1, 65535, 65536, 65537, 131071, 131072, 131073, 67_108_864_000}` とその符号反転で見る。65536 は 128、131072 以上は 255。
- `WorldQ` と `C` の合成、`frob * upem^2`、`place_q`、直接帯の点を、上の各上限の端点（`{-(2^31 - 1), -1, 0, 1, 2^31 - 1}` と `{0, ±2^60}`、`upem ∈ {1, 1000, 2048, 16384}`）の直積で checked に計算する。i128 の結果が i64 に戻る箇所（`C`、`G`）は、上限の検査で None になる入力だけが i64 を超えること。
- `place_q` の位相。`C.t` の `{-3 * 2^27, -2^27, -1, 0, 1, 2^27, 3 * 2^27}`（−0.375、−0.125、…、0.375 画素）で、`(origin, phase)` が `(-1, 2)`、`(-1, 3)`、`(0, 0)`、`(0, 0)`、`(0, 0)`、`(0, 1)`、`(0, 2)` になること。`-2^27` の結びは `-0.25` へ行き、v2 の `place` の `(0, 0)` と違うことを表明する。
- 着色と重ねは `cpu_composite.rs` の `glyph_q14_does_not_overflow`（新規）。`b` の 256 通りすべて × `c_a ∈ E` × `c_rgb ∈ E` × `acc` の `rgb <= a` の `E` の組（55 組）で、`out_a <= 16384` と `out_rgb <= out_a` を表明する。`out_a > 16384` か `out_rgb > out_a` になったら失敗する。`E` は #24 の `{0, 1, 4095, 4096, 4097, 8191, 8192, 8193, 16383, 16384}` である。

単体テストはほかに四つ足す。

- `rasterizer_q8_matches_exact_area`。v2 の `rasterizer_matches_exact_area` と同じ正方形と円で、3/4 被覆の列が 191、内側が 255、円のインクが πr² の 1% 以内。
- `rasterizer_q8_window_crop`。一つの字形を大きな窓で描いた結果と、それを 2×2 に割った四つの窓で描いた結果の切り出しが全バイト一致する。窓の境が字形の中を通る場合、曲線の分割点が境の上に乗る場合、窓の左端が字形の点の 1 画素右にある場合（「カバレッジ」の最後の段落の `P0 = (255, 0)`、`C = (256, 15)`、`P1 = (255, 0)`）、字形の広がりがちょうど `2^24` の場合（どの窓でも描かれる）と `2^24 + 1` の場合（どの窓でも描かれない）を含む。
- `atlas_state_does_not_change_v3_pixels`。`GlyphAtlas::new` を小さくして満杯にし、追い出しと None を起こしても、`use_atlas` を偽にしても、ノードの Q14 が通常のアトラスの場合と全バイト一致する。
- `glyph_key_separates_raster_versions`。同じ字形と行列で `raster` が 0 と 1 の鍵が別のエントリになり、v3 で形の違う輪郭（`outline_hash` が違う）が同じ `(AssetId, glyph)` から来ても別のエントリになる。
- `text_bake_roundtrip_and_validation`（`alto-engine`）。焼き込みの直列化が往復し、上限を一つずつ外したブロブが `TEXT_BAKE_INVALID` になる。
- `text_bake_ignores_asset_replacement`。焼いたあとで `SetAsset` が同じ `AssetId` に別のフォントを置いても、焼き直しの `SetText` がない限り v3 の画素が変わらない。エンジンが同じ取引で焼き直すことも見る。暖まったエンジンと開き直したエンジンで同じ画素になること。

## 一致の期待

一致は全バイト一致である。バックエンドは #24 と同じ三つである。Metal は native ジョブ（`Apple Paravirtual device`、実機は未検証）、lavapipe は gpu-parity ジョブ、SwiftShader は wasm-browser ジョブ（PR #30、main の `db6e471` で wasm から分かれた。アダプタの architecture に `swiftshader`）。ブラウザは `tests/fixtures/render-v3-empty.alto` を開いて v3 の文書にし、そこへテキストを足す。テキストの追加は焼き込みを作り、その blake3 は `tests/parity/text-bake.txt` と一致しなければならない（上の「依存の版」）。

一致を期待する理由は一つである。描画の入力は焼き込みのバイトと文書の node 変換だけで、node 変換はビット列の整数分解で Q16 に入る。その先は CPU も GPU も整数で、GPU は `unorm_byte` のあと整数の加減乗と非負の除算だけを行い、上の表で溢れがないことを示した。描画に浮動小数の算術はない。

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
| `.notdef` | `AltoTestLatin.ttf` だけで "Aあ"。"あ" はそのフォントの字形 0 | 一致 | 一致 | 一致 | 一致 | enforce。`text-v3-notdef` |
| フォントなし | スタイルの `fonts` が空で "Ag"。プレースホルダの箱 | 一致 | 一致 | 一致 | 一致 | enforce。`text-v3-nofont` |
| 負の位相の結び | 12px の "A" を原点 `(-0.125, 80)` の node 変換で、タイル (-1, 0) と (0, 0) | 一致 | 一致 | 一致 | 一致 | enforce。`text-v3-negative-phase` |
| v2 の `text` | `createDocument` のまま | — | — | — | — | xfail のまま |
| v2 の `text` の計測 | 同じ "Ag" を v2 で、3 バックエンドの差の画素数 | — | 計測 | 計測 | 計測 | measured-not-enforced |

Rust の参照は、焼き込みのブロブと node 変換だけから描く。`TextRun` も `layout` も読まない。シーンの組み方は `plain_scene_gaps_are_explicit` が `RContent::Text(Arc::new(Vec::new()))` を置くのと同じで、v3 では `RContent::Text` に焼き込みのハッシュが付く。焼き込みのバイトは `tests/parity/text-bake.txt` が固定したものである。JS は上の `textBake` を使う。

テキストの新しいケースはレベル 0 だけにする。これはこの文書の制限で、parity 全体の性質ではない（`parityCases` にはレベル 1 の `level1-centers` があり、`readTileLevel` で読む）。レベルが違うと `em_px` が変わり、帯が切り替わる。レベル 1 と負のレベルのテキストのケースは、レベル 0 が固定されてから足す（「未解決の問題」の 10）。

ケースの名前は `text-v3-` で始める（`text-v3-ag`、`text-v3-small`、`text-v3-vertical`、`text-v3-overlap`、`text-v3-rotated`、`text-v3-atlas-off`、`text-v3-seam`、`text-v3-notdef`、`text-v3-nofont`、`text-v3-negative-phase`）。

## enforce と measure

enforce するのは、構成上一致し、3 バックエンドの CI で全バイト一致したケースだけである。上の表で enforce と書いたものは、実装 PR で一致を確かめてから通常のケースとして入れる。期待どおりに一致しなかったら、measure に落とさない。設計に戻る（#24 のアトラス読み取りと同じ扱い）。`R8Unorm` のアトラス読み取りは、入れた時点から必須である。

measure するのは、v2 の `text` と、テキストに v3 がまだない経路（マスク、効果）を重ねたケースである。`measureRow` と `formatMeasure` で `measure: true, enforced: false` の行にし、失敗にしない。v2 の計測は「前提」の 1 から 4 のどれが効いているかを分けるため、`setGlyphAtlas(false)` の行と 12px の行も持つ。

## v1 / v2 を動かさない

- `glyph-atlas.wgsl`、`vector.wgsl`、`vector-paint.wgsl`、`rasterize`、`accumulate`、`place` の文は一文字も変えない。v3 は `place_q`、`rasterize_q8`、`glyph-q14.wgsl`（いずれも新規）を使い、`place` と `max_scale` を呼ばない。
- `NodeKind::Text` と `SetText` の `bake` は v1 / v2 で常に None で、直列化では省く。v1 / v2 の文書の JSON、内容ハッシュ、レイアウトのキャッシュは変わらない。v1 / v2 は今の `text_entry` と `OutlineCache` と `render_text` で描く。
- `GlyphKey` の `raster` と `outline_hash` は v1 / v2 で 0 である。鍵の型が変わるだけで、v2 のビットマップの中身と置き場所の規則は変わらない。
- ホストの分岐は `self.scene.render_version >= alto_core::RENDER_VERSION_EXACT` である。`render_text` は v2 の経路として残し、v3 は `raw_node` の `RContent::Text` から新しい `text_q14`（`m5.rs`）を呼ぶ。
- v2 の「アトラスが満杯ならベクター経路」と `setGlyphAtlas` の効果は v2 に残す。v2 はこの既知の非決定性を持ったまま凍結する。既存文書の対処は `render.upgrade` である。
- `tests/parity/render-v1.txt` と `tests/parity/cpu-reference.txt` の既存行は一文字も変えない。v3 の行を足すだけである。ただし今の `existing_v1_and_v2_hashes_are_unchanged`（`cpu_composite.rs`）は、`cpu-reference-v2-before-alpha.txt` より後の行に `-alpha-measure ` を含むことを要求し、`render-v1.txt` 全体が `render-v1-before-alpha.txt` と等しいことを要求する。`text-v3-` の行はそのままでは落ちる。実装順の 6 で、後ろに足せる行を「`-alpha-measure ` を含む、または名前が `text-v3-` で始まる」に広げる。前置の一致と v1 の全体の一致の検査は残す。v2 のテキストにはロック済みのハッシュがない（xfail）が、出荷済みの v2 の文書の画素は、文を変えないことで守る。
- `plain_scene` と `gap` は変えない。テキストは v3 でも合成パスに乗らない。
- ゲートは今のまま使う。Rust は `allow_unstable_render_v3`（`crates/alto-engine/src/unstable_render.rs`）で、デバッグビルド（`cfg!(debug_assertions)`）、コンパイル時の許可（このクレートのテストかフィーチャ `unstable-render-v3`）、実行時の要求の三つがすべて要る。リリースの成果物はフィーチャがあっても拒否する。JS は `renderVersionUpgrade`（`packages/alto-sdk/src/render-version.ts`）で、`{ unstableRenderV3: true }` を渡しても、ビルドの許可 `buildEnabled`（既定は出荷定数 `UNSTABLE_RENDER_V3` で false）がなければ `rejected` になる。オプションだけでは v3 にならない。
- alto-text のコードは変えない。変えるのは `Cargo.toml` の `rustybuzz` の版の固定（`"0.20.1"` から `"=0.20.1"`、`Cargo.lock` は同じ）だけで、v1 / v2 の画素は変わらない。焼き込みは `alto-engine` が `layout` の結果から作る。
- シェーダが変わるのでリリース WASM のバイナリのハッシュとサイズは動く。PR #29（main の `ffdc1bf`）以降、`wasm/SHA256SUMS` と `verification/build.json` は各 PR では更新しない。`RELEASING.md` のとおり、タグの前に main の `wasm-hashes-main` から一度だけ更新し、main ではずれは報告だけである。描画のハッシュとは独立である。

## 実装順

前提は #26 の「実装順」の (b)（v3 composite）と (c)（M5 の Q14 中間）である。(a) の measure-only は PR #28（main の `feb1d67`）で入った。テキストはそれに依らない。テキストはマスク (d) と独立で、(c) のあとならどちらが先でもよい。各項目は main から切った一つの PR である。

0. **焼き込み。** `alto-doc` の `NodeKind::Text` と `SetText` に `bake`、`validate` の検査。`alto-engine` の `bake_text`、`text_color_q14`、焼き込みの形式、`SetAsset` での同じ取引の焼き直し、`render.upgrade` での焼き込み。`rustybuzz` の `=0.20.1`、`text_bake_dependencies_are_pinned`、`text_bake_roundtrip_and_validation`、`text_bake_ignores_asset_replacement`、`tests/parity/text-bake.txt`。描画は変えないので、画素は変わらない。
1. **ラスタ化の核。** `glyph_atlas.rs` に `place_q`、字形の原点、分割数の判定、`flatten_q8`、`rasterize_q8`（窓を受ける）を足す。単体テスト `rasterizer_q8_matches_exact_area`、`rasterizer_q8_window_crop`、`text_q8_raster_does_not_overflow`。どこからも呼ばないので、画素は変わらない。
2. **アトラスの鍵と `R8Unorm` の読み取り検査。** `GlyphKey` に `raster` を足し、`glyph_key_separates_raster_versions`。`Gpu::atlas_read` と `measureAtlasRead` に `R8Unorm` の 256 値を足し、必須にする。v2 の画素は変わらない。
3. **Rust の参照。** `text_q16_from_bits` と `q_from_float_bits_rounds_half_away` を追加する。 `cpu_composite.rs` に `text_q14` と `glyph_q14_does_not_overflow`。焼き込みの読み、`WorldQ` と `C`、帯の選択、アトラス帯と直接帯、着色と重ねを、本番の `place_q` と `rasterize_q8` の上に書く。
4. **WGSL とホスト。** `glyph-q14.wgsl`、`GlyphQ14` のインスタンス、スクラッチの `R8Unorm`、チャンクのディスパッチ、`m5.rs` の `RoiQ` と `WorldQ` の受け渡し、`text_q14` と分岐。`glyph_q14_uses_effects_helpers` と `atlas_state_does_not_change_v3_pixels`。native の 2 バックエンドで、上の enforce のケースを Rust の参照と比べる。
5. **JS の参照。** wasm の `textBakeForTesting`、worker の `textBake`、`cpu-composite.js` の焼き込みの読み、`placeQ`、`rasterizeQ8`、`renderTextV3`（BigInt、`divQBig`）。`textQ16FromBits` と Rust と同じビット列の境界値テストも追加する。
6. **固定。** enforce のケースを 3 バックエンドで流し、一致を記録してから v3 のハッシュとして `cpu-reference.txt` に `text-v3-` の行を足す。同じ PR で `existing_v1_and_v2_hashes_are_unchanged` の追記の規則を広げる（上の「v1 / v2 を動かさない」）。各ジョブは CPU 参照との比較に加え、焼き込みの blake3（`text-bake.txt`）と描画のハッシュ（`cpu-reference.txt`）の両方に一致しなければならない。Metal と lavapipe は native で、SwiftShader は wasm で焼くので、焼き込みの native と wasm の一致もここで見る。
7. **measure の行。** v2 の `text` の計測と、マスクや効果を重ねたケース。

## 未解決の問題

1. **アトラス帯の量子化の画質。** アトラス帯は今と同じく、行列の四つの係数を 1/4 px/em の帯に、原点を 1/4 px の位相に量子化する。決定性には関係しないが、直接帯との境（`em_px = 24`）で字形が跳ぶ。跳びに一律の上限はない。位相の誤差は軸ごとに 1/8 px 以下だが、係数の誤差は係数ごとに `1 / (8 upem)` px／フォント単位で、点のフォント単位の座標 `(x, y)` に比例する。一つの軸で `(|x| + |y|) / (8 upem) + 1/8` px まで足し合わさり、`x = upem` の点は係数 a だけで 1/8 px 近く、位相とで 1/4 px 近く動く。字形の大きさで変わる。境で跳ばないように帯をやめ、全字形を直接帯にするか（アトラスの再利用を捨てる）は決めていない。
2. **直接帯の CPU の費用。** 72px 以上の大きな字形は、タイルごと、レベルごとに CPU でラスタ化し直す。タイルのキャッシュが内容ハッシュで再利用するが、ズーム中の初回の描画は遅くなり得る。窓をまたいだ字形のキャッシュ（窓の独立性があるので、字形ごとの大きなビットマップを切り出してよい）を足すかは、計測してから決める。
3. **整数の整形器。** 決定済みの部分：v3 の描画は焼き込みのバイトだけを読み、整形器の版に依らない（「数の境」「依存の版」）。開いている部分：焼き込みを作る側も整数の整形と配置（rustybuzz の整数の位置、Q24 の配置）に替え、同じ編集を二つのピアが別々に焼いても同じバイトにするかは決めていない。替えても描画の式と既存文書の画素は変わらない。
4. **ヒンティングを将来入れるか。** 小さい文字の画質のために、整数で決定的なヒンティング（たとえば y 方向だけの整数スナップ）を v4 で入れる余地はある。v3 には入れない。
5. **可変フォント。** main にない。入れるなら、F2Dot14 の差分の補間を整数で行い、Q8 の前の f32 を作らない規則が要る。
6. **巨大な字形。** 広がりが約 65536 画素を超える字形（字形の座標で `2^24` を超える点を持つもの）は v3 で描かない。判定は字形だけで決まり、窓に依らない。極端なズームで実際に現れるかは調べていない。現れるなら、点の上限を i128 で広げるか、直線の切り詰めを先に行う。
7. **焼き込みの大きさ。** 焼き込みは字形の輪郭を文書へ複製する。長い文章では、フォントのバイトより大きくなり得る。輪郭を `(font hash, face_index, glyph)` ごとの別ブロブに分けて共有するかは、実装 PR で大きさを計測してから決める。形式の `version` を上げれば足せる。
8. **チャンクと binning。** `GLYPH_Q14_CHUNK` の 1024 と、画素ごとに全インスタンスを見るループは、文字の多いタイルで遅い。16×16 のビンに分けても順を保てば結果は変わらないが、いつ入れるかは計測してから決める。
9. **古いメモの指し先。** [正確な CPU 参照](2026-10-27-exact-cpu-references.md) の「text」節は、この文書で置き換えた。あの節からこの文書への指し先は、実装 PR で足す。この変更は文書を一つ足すだけで、既存の文書は触らない。
10. **レベル 0 以外のケース。** テキストの新しい parity はレベル 0 だけである。帯の切り替わり（`frob * upem^2` の条件がレベルで変わる）をレベル 1 と負のレベルで固定するケースを、いつ足すかは決めていない。
