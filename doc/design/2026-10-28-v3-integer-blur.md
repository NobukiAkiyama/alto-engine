# v3 整数ガウスぼかし

作成日：2026-10-28（Asia/Tokyo）

状態：設計のみ。この文書は実装しない。ガウスぼかしの v3 は、[v3 整数合成](2026-10-28-v3-integer-compositing.md)（PR [#24](https://github.com/NobukiAkiyama/alto-engine/pull/24)、main の `5ce300e`）と [v3 整数マスク](2026-10-28-v3-integer-mask.md)（PR [#26](https://github.com/NobukiAkiyama/alto-engine/pull/26)、main の `7309b61`）と同じ不安定オプトインの向こうに置く。描画バージョン 1 と 2 のぼかしの式（`exp` の f32 の重み、`rgba16float` の中間、f32 の双線形の拡大）は凍結したままである。

関連：[v3 整数合成](2026-10-28-v3-integer-compositing.md)、[v3 整数マスク](2026-10-28-v3-integer-mask.md)、v3 整数テキスト（PR [#31](https://github.com/NobukiAkiyama/alto-engine/pull/31)、未マージ。`doc/design/2026-10-28-v3-integer-text.md`）、[正確な CPU 参照](2026-10-27-exact-cpu-references.md)、[画像編集](IMAGE-EDITING.md)、[半透明合成の計測](../verification/semi-transparent-composite-parity.md)。

## 前提

ぼかしは合成パス（`composite.wgsl`）を通らない。`plain_scene`（`crates/alto-scene/src/damage.rs`）は効果を持つシーンを外し、M5 の効果パス（`crates/alto-gpu/src/renderer/m5.rs`、シェーダは `crates/alto-gpu/shaders/effects.wgsl`、中間は `rgba16float`）が描く。Rust の `gap`（`crates/alto-gpu/src/cpu_composite.rs`）と JS の `sceneGap`（`tests/e2e/cpu-composite.js`）は `"effects"` を返し、parity の `gaussian-blur`（`tests/e2e/parity-tests.js` の `xfail("gaussian-blur", …)`）は xfail である。

### 文書の値

| 段 | 場所 | 今の扱い |
| --- | --- | --- |
| 文書 | `Effect::GaussianBlur { radius: f64, .. }`（`crates/alto-doc/src/lib.rs`） | 検証は `finite("radius", *radius, 0.0, 64.0)`。省略時は 8 |
| シーン | `render_effect`（`crates/alto-engine/src/m5/mod.rs`） | `n as f32` で `REffectKind::GaussianBlur { radius: f32 }`（`crates/alto-scene/src/lib.rs`）へ |
| ベクターマスクの feather | `VectorMask::feather: f32`（`crates/alto-vector/src/lib.rs`） | 検証は `0.0..=64.0`。`path_coverage`（`m5.rs`）が同じ `REffectKind::GaussianBlur { radius: feather }` を作る |

### M5 の流れ

| 関数（`m5.rs`） | 役割 | 今の算術 |
| --- | --- | --- |
| `Roi::expand` | ROI を左右上下に整数画素だけ広げる | `x - n as f32 * scale`、`w + 2n`。f32 |
| `Roi::coarsen` | 半径が大きいとき、粗い倍率で描き直す | `scale.max(2f32.powf(minimum.max(0.000001).log2().ceil()))`。原点は `(x / scale).floor() * scale - scale`、両端に 1 標本ずつ足す |
| `input_roi` | 効果の列を後ろから辿り、各ぼかしの `r = (radius / roi.scale).ceil() as u32` だけ `expand(r, r)` する | f32 の `ceil` |
| `effects` | 出力 ROI を後ろから決め（ぼかしごとに `expand(r, r)`）、前から `stage` を呼ぶ | 同上 |
| `stack` | 効果のスタック。末尾がぼかしなら `desired.coarsen(radius / 64.0)` の `sample` で描き、`stage` し、`crop` で `desired` へ戻す。`selection` があれば元を `desired` で描き直し、モード 5 で混ぜる | 上の f32 |
| `stage` | ぼかしは `r == 0` なら素通し。そうでなければ `middle = alloc(roi.expand(0, r))` へ水平（`info.w = 0`）、`out = alloc(roi)` へ垂直（`info.w = 1`）。`settings = [(radius / roi.scale / 3.0).max(0.333333), 0, 0, r as f32]` | f32 の uniform |
| `crop` | ROI が同じなら素通し。違えばモード 8 | — |
| `children` | 調整レイヤーのスタックに半径 0 より大きいぼかしがあれば、兄弟を `StackInput::Siblings` で描き直して背面のハローを作る | 上と同じ `stack` |
| `path_coverage` | ベクターマスクの feather。`roi.coarsen(feather / 64.0)` で描いたカバレッジに `effects` で同じぼかしを掛け、`crop` | 同上 |

### シェーダ（`effects.wgsl`）

- **モード 2。** 画素中心 `p = c.out_region.xy + (vec2f(id.xy) + 0.5) * c.out_region.z`。`i = -64..=64` を回し、`abs(i) > radius` は飛ばす。重みは `exp(-f32(i*i) / (2.0 * sigma * sigma))`、`sigma = max(c.settings.x, 0.333333)`。位置は `load_a(p + delta)`、`delta = vec2f(f32(i) * c.out_region.z, 0)`（`c.info.w == 1u` なら `.yx`）。和を重みの和 `total` で割る（`sum / max(total, 0.000001)`）。
- **`load_a`。** `vec2i(floor((p - c.region_a.xy) / c.region_a.z))` が `[0, sizes.xy)` の外なら 0（透明）。
- **モード 8。** `load_a(p)`。入力の倍率が粗い（`c.region_a.z > c.out_region.z`）なら `a_linear(p)`。`a_linear` は `q = (p - region_a.xy) / region_a.z - 0.5`、`floor` と `fract` の f32 の `mix` で 4 タップ、範囲外のタップは `a_at` が 0 を返す。

### JS の今の参照

JS にぼかしの CPU 参照はない。`tests/e2e/m5-tests.js` の `weights` と `intervalBlur` は、f64 の `Math.exp` で同じ重み（`r = Math.ceil(radius)`、`sigma = Math.max(radius / 3, 0.333333)`）を作り、矩形の分離した積を許容誤差つきで比べる（"Gaussian blur matches CPU reference across both tile seams and preserves source"）。ビット一致の参照ではない。

### 今日のずれ

1. **`exp`。** WGSL の `exp` の精度は `3 + 2 * |x|` ULP で、正しい丸めは要求されない。Metal、lavapipe、SwiftShader で重みのビットが揃う理由はない。CPU の libm を写しても、三つの GPU に同時には合わない。
2. **f32 の和と除算。** `sum += load_a(...) * weight` と `sum / total` は f32 である。融合の有無と順は実装が決める。
3. **`rgba16float` の中間とストア。** 水平パスの結果と出力は f16 に丸めて書かれ、最後に `effects-resolve.wgsl` が `rgba8unorm` へ `textureStore` する。#24 が決定性の基礎にしないと裁定したものそのものである。
4. **拡大の `a_linear`。** 位置と重みは下で示すとおり 2 進の有限小数で正確だが、`mix` は f32 で、f16 の値を補間する。
5. **ホストの `coarsen`。** `f32::log2` と `f32::powf` は libm である。macOS の libm、glibc、wasm の Rust 同梱の libm は別の実装で、正しい丸めを要求されない。`radius / 64` がちょうど 2 の冪のときに `log2` が 1 ULP ずれると `ceil` が変わり、粗い倍率が 2 倍違う。ROI と画素が native と wasm で分かれ得る。v2 はこの既知の非決定性を持ったまま凍結する。

[正確な CPU 参照](2026-10-27-exact-cpu-references.md) の「gaussian-blur」節は、`exp` を Q16 の整数多項式で WGSL でも評価し、重み和の切り捨て除算で正規化し、f16 ビットへ書く案だった。この文書がそれを置き換える。理由は三つある。#24 は f16 / unorm の中間を v3 から除いた（#26 の改訂で、中間は `rgba16uint` の Q14）。切り捨て除算は #24 の「結びは 0 から遠い側」の規則と違う。重みは画素に依らないので、WGSL で評価する理由がない（下の「決定」）。

## 決定：重みはホストが作り、GPU は整数の積和だけ

v3 のぼかしの重みは、ホスト（Rust と JS）が整数だけで作る。WGSL は重みを計算しない。uniform で受け取った整数の重みと、`rgba16uint` の Q14 の積和、2 の冪での丸めだけを行う。

理由は三つある。

1. **重みは画素に依らない。** 重みは `(r, ρ')`（下の「半径の量子化」）だけで決まり、一つの `stage` のすべての画素で同じである。今のモード 2 は、出力の画素ごと、パスごとに `2 * min(r, 64) + 1` 回 `exp` を呼ぶ（ループは 129 回だが、`abs(i) > radius` は `exp` の前で飛ばす。レベル 0 の半径 8 で 17 回、`r >= 64` で 129 回）。これは速度のためでも精度のためでもない。
2. **一致の対象が減る。** 重みの生成は Rust の一つのソースで、native（Metal、lavapipe）と wasm（SwiftShader）が同じコードをコンパイルする。三つの GPU が受け取る重みは定義上同じである。参照として独立なのは JS である。
3. **WGSL に `exp` も多項式も置かない。** 多項式を WGSL で評価すると、Q32 の積（u64）を `Wide` で書くことになる。三実装のうち最も保ちにくい実装に、いちばん溢れやすい部分を持たせない。

三実装の分担は次のとおりである。

| 部分 | WGSL | Rust | JS |
| --- | --- | --- | --- |
| 半径の量子化、`r`、粗い倍率 | — | 本番（`m5.rs`、参照も兼ねる） | 参照（`cpu-composite.js`、新規） |
| 重みの生成と正規化 | — | 本番 `blur_kernel_q16`（`renderer.rs`、新規。参照も兼ねる） | 参照 `blurKernelQ16`（新規） |
| 水平と垂直の積和、拡大 | 本番（(c) の v3 の効果シェーダの新しいモード） | 参照（`cpu_composite.rs`、テストのみ、新規） | 参照（同上） |

## 範囲

| 対象 | 対象外 |
| --- | --- |
| レイヤーの効果 `REffectKind::GaussianBlur`（`stack` の末尾、効果の列の途中、直列の複数のぼかし） | 効果の `selection` を掛けるモード 5。#26 の範囲。ぼかしはその入力 `f` を作るだけ |
| 調整レイヤーの背面ぼかし（`children` の `StackInput::Siblings`） | 背面と親への合成。#24 と #26 の (c) の範囲 |
| ベクターマスクの feather（`path_coverage`）のぼかしの段 | feather の入力のカバレッジ（vector-fill の v3）と、それを掛けるモード 11 と 12（#26） |
| 粗い倍率での描き直し（`coarsen`）と、`desired` への拡大（`crop`） | 他の効果（levels、curves、hue-saturation、invert）。color-adjust は (c) |

## main と #24・#26 から使うもの

新しい丸めは作らない。結びは 0 から遠い側へ行く。ぼかしの項はすべて 0 以上なので、実際に通るのは「除数の半分を足して切り捨てる」側だけである。

| 名前 | 場所 | ここでの役割 |
| --- | --- | --- |
| `div_q_i64` / `divQBig` | `cpu_composite.rs` / `cpu-composite.js` | 重みの生成の非負の除算。Rust の本番の `blur_kernel_q16` は同じ式の u64 / u128 版を持つ（下の「重みの生成」） |
| `bilinear_byte`、`channel_byte`、`bilinearByte` | `crates/alto-gpu/src/sample.rs`、`composite.wgsl`、`cpu-composite.js` | 拡大の 4 タップの式の形。v3 は同じ形を Q14 の値に使う |
| `Roi::expand`、`Roi::coarsen`、`input_roi` | `m5.rs` | ハローの大きさと粗い倍率。v3 は同じ意味を整数の `RoiQ` で書く（下の「浮動小数の境」）。v2 の文は変えない |
| `premul_byte_q14`、`byte_q14` | `effects.wgsl`、`cpu_composite.rs`、`cpu-composite.js` | ラスターの入口（モード 18 の置き換え先、(c)）。ぼかしの式では使わない |
| `colorAdjustGradient` / `color_adjust_gradient` | `cpu-composite.js` / `cpu_composite.rs` | 全 RGB と、アルファ 0、1、128、254 を持つ入力画像 |
| `measureRow`、`formatMeasure` | `parity-tests.js` | measured-not-enforced の行 |
| `RENDER_VERSION_EXACT` | `crates/alto-core/src/lib.rs` | `m5.rs` の `blend`、`stage`、`raw_node` と同じ分岐 |

前提の PR は二つある。#24 の設計を実装する v3 composite の PR #27（未マージ。`composite-q14.wgsl` は main にまだない）と、#26 の「実装順」の (c)（M5 の v3 の中間を `rgba16uint` にし、タイル出力を `div_q_i64(q * 255, 16384)` の `rgba8uint` にする）である。(c) は `rgba16uint` を読むために `texture_2d<u32>` の束縛を持つ v3 の効果シェーダを入れる。この文書ではそれを「(c) のシェーダ」と呼ぶ。ぼかしはそこへモードを足し、別のシェーダを書かない。

## 値の置き場

入力、水平パスの結果（`middle`）、出力は、いずれもプレマルチプライ Q14（0..=16384）で、(c) の `rgba16uint` のプールに置く。読むのは `textureLoad` だけ、書くのは `textureStore` の整数だけである。`rgba16float`、`textureSample`、サンプラは使わない。

`middle` を Q14 に丸めて `rgba16uint` に置く。丸めない和（最大 2^30）を `rgba32uint` に置けば、二回の積和を一回の丸めにでき、パスの順にも依らなくなる。それは採らない。理由は三つある。プールが二つの形式になる（`alloc` の `w * h * 8` の予算も二通りになる）。二つ目のパスの和が 2^46 になり、WGSL で `Wide` が要る。丸めの差は Q14 で 1 以下で、出力のバイト（Q14 で 64 刻み）より二桁小さい。代わりに、パスの順を規定する（下の「二つのパス」）。

## 浮動小数の境

v3 のぼかしの中に浮動小数はない。ホストの算術も含む。浮動小数が現れるのは、文書に保存された値そのもの（JSON の f64 の `radius`、`VectorMask` の f32 の `feather`）だけで、v3 はそれを**ビット列から整数として読む**。浮動小数の演算（`/`、`*`、`ceil`、`log2`、`powf`、`Math.fround`）は一つも通さない。今の `render_effect` の `n as f32` と、`stage` の `radius / roi.scale`、`coarsen` の `log2` / `powf` は v2 の経路に残す。

丸めは #24 の一つだけである（非負なので「除数の半分を足して切り捨てる」）。そのほかに出てくるのは、正確な有理数の床と天井（`floor_div`、`ceil_div`）だけで、これは結びのある丸めではない。v2 の ROI の `floor` と `ceil` を整数で同じ意味に書いたものである。`floor_div(a, b)` は `b > 0` で負の `a` でも床（Rust の `div_euclid`、JS の BigInt では負のときに補正する）、`ceil_div(a, b) = -floor_div(-a, b)` である。

### 半径の Q8

文書の半径を一度だけ Q8 の整数 `R`（0..=16384、1/256 画素）にする。新しい関数 `blur_radius_q8_f64`（Rust は `crates/alto-engine/src/m5/mod.rs`、JS は `blurRadiusQ8`）で、定義は実数の `floor(radius * 256 + 1/2)` である。実装はビットだけで行う。

```text
bits = f64 のビット列（Rust は to_bits、JS は DataView.setFloat64 と getBigUint64）
e    = (bits >> 52) & 0x7ff;   frac = bits & (2^52 - 1)
e == 0 なら R = 0                       // 0 と非正規化数。radius < 2^-1022
m = frac | 2^52;  s = 1067 - e          // radius * 256 = m * 2^(-s)
s <= 0 なら R = m << (-s)
s >= 54 なら R = 0                      // m * 2^-s < 1/2
それ以外  R = (m + 2^(s - 1)) >> s
```

文書の検証（`finite("radius", *radius, 0.0, 64.0)`）が先に通っているので、符号ビットは 0（`-0.0` は `e == 0` で 0）、`R <= 16384` である。feather は f32 なので、同じ手順を `e = (bits >> 23) & 0xff`、`m = frac | 2^23`、`s = 142 - e`、`s >= 25` なら 0 で行う（`blur_radius_q8_f32`、JS は `blurRadiusQ8F32`。JS は文書のスナップショットの数値を `DataView.setFloat32` で f32 のビットにする。f32 の値は f64 に正確に入っているので、この変換は丸めない）。

`REffectKind::GaussianBlur` に `radius_q8: u32` を足す（`crates/alto-scene/src/lib.rs`）。`render_effect` と `path_coverage` が作る。v2 は今の `radius: f32` を読み続ける。

### 整数の ROI

v3 のぼかしは、`Roi`（`m5.rs`、f32 の `x`、`y`、`scale`）ではなく、整数の `RoiQ { level: i32, ox: i64, oy: i64, w: u32, h: u32 }` で ROI を計算する。型と意味は改訂後の #31（v3 整数テキストの「座標の系」）の `RoiQ` と同じで、ホストは `compose_m5` → `node` → `raw_node` の同じ再帰で `RoiQ` を運ぶ。二つの文書は同じ一つの型を使い、別の版を作らない。文書座標は `x = ox * 2^level` で、`ox` はそのレベルの画素の番号である。タイル `(level, tx, ty)` の ROI は `ox = tx * 256`、`oy = ty * 256`、`w = h = 256` で、`compose_m5` の f32 の式 `key.1 as f32 * 256.0 * scale` を整数で書いたものである。

| v2（`Roi`、f32） | v3（`RoiQ`、整数） |
| --- | --- |
| `expand(x, y)`：`x - n * scale`、`w + 2n` | `ox - n`、`oy - n`、`w + 2n`、`h + 2n` |
| `coarsen`：倍率 `S = 2^L`、`x' = floor(x / S) * S - S`、`w' = ceil((x + w s - x') / S) + 1` | `m = L - level`、`ox' = floor_div(ox, 2^m) - 1`、`w' = ceil_div(ox + w - ox' * 2^m, 2^m) + 1`（y も同じ）。#31 の `ox' = floor_div(X, 2^L') - 1`、`w' = ceil_div((ox + w) * 2^L - ox' * 2^L', 2^L') + 1`（`X = ox * 2^L`）を `2^level` で割ったもので、同じ値である |

v2 の f32 の式が正確で、両者が同じ ROI になるのは、途中のすべての値が f32 で正確なときである。原点だけでは足りない。`coarsen` は端点 `x + w * scale` も計算するので、レベルの画素で `|ox|`、`|ox + w|`、`|ox' * 2^m|`、`|ox' * 2^m + w' * 2^m|`（y も同じ）がすべて `2^24` 以下であることを要る。反例：レベル 0、`ox = 16_776_959`、`w = 258`（`ox = 16_776_960` のタイルを 1 画素広げたもの）をレベル 1 へ粗くすると、整数は `ox' = 8_388_478`、`w' = 132` だが、v2 の f32 は端点 `16_777_217` を `16_777_216` に丸めて `w' = 131` になる。v3 の式は範囲に依らず正確である。

v3 の描画に浮動小数の演算は置かない（#31 のレビューの規則。ROI と座標を含む）。ぼかしの段は f32 の `Roi` を作らず、受け取らない。受け取り、渡すのは `RoiQ` と Q14 の画素だけである。ぼかしの入力を描く段も `RoiQ` を受け取る。テキストは #31 がそう定義した。ラスターの入口（モード 18 の置き換え先）は (c) と #26、ベクターは vector-fill の文書が、`RoiQ` から整数で位置を作る。それが入るまで、その入力を持つぼかしのケースは enforce しない（「一致の期待」）。

### 半径の量子化

`stage` ごとに、その出力 `RoiQ` のレベル `l` と `R` から、三つの整数を作る。

```text
rho8 = l >= 0 ? div_r(R, 2^l) : R << (-l)          // その倍率での半径の Q8
r    = l >= 0 ? ceil_div(R, 256 * 2^l) : ceil_div(R << (-l), 256)   // タップの半幅
rho' = max(rho8, 256)                               // σ = rho' / 768 画素。σ >= 1/3
```

`div_r(n, d) = (n + d / 2) / d`（`n >= 0`、`d > 0`）は #24 の非負の丸めで、結びは上へ行く。`r` は v2 の `(radius / roi.scale).ceil()` を、量子化した `R` の正確な有理数で書いたものである。

- `R == 0` なら `r = 0` で、今と同じく素通しである。重みを作らない。v2 の素通しは `ceil(f32(radius) / scale) == 0` のとき、すなわち f32 に縮めた半径が 0 のとき（`radius == 0` と、`radius = 2^-150` のように f32 で 0 に落ちる正の値）だけである。f32 で正の半径は、どのレベルでも v2 では `r >= 1` で僅かにぼける。v3 は `R == 0`、すなわち `radius < 1/512` を素通しにする。
- `r` は `stage`、`effects`、`input_roi` の三か所で同じ関数 `blur_taps(R, level)`（`m5.rs`、新規）から取る。ハローの大きさと、パスが読む幅が、いつも同じ整数になる。
- v2 の `sigma = max(radius / scale / 3, 0.333333)` は、v3 では `σ = rho' / 768` である。下限は 1/3 ちょうどで、0.333333 ではない。v3 は新しいハッシュなので、画素の互換性の問題にならない。

### 粗い倍率

v2 の `coarsen(radius / 64)` は、倍率を `2^ceil(log2(radius / 64))` 以上にする。v3 は同じ意味を整数で書く。`radius / 64 = R / 2^14` なので、

```text
clog2(R) = R <= 1 ? 0 : bit_length(R - 1)          // 2^k >= R となる最小の k
L = max(level, clog2(R) - 14)                        // R >= 1 のとき
```

である。`bit_length` は Rust の `u32::BITS - (R - 1).leading_zeros()`、JS は `(R - 1).toString(2).length` である。`R == 0` なら粗くしない（`r = 0` で素通しなので、v2 の `minimum.max(0.000001)` の下限は要らない）。`L == level` なら `coarsen` は ROI をそのまま返す。v2 の `if scale <= self.scale { return self; }` と同じである。

**v3 の定義として明記する（#31 レビューの Linus の P3）。** v3 のぼかしの粗い倍率 `k` は、上の通り **Q8 に丸めた半径 `R` から** `k = clog2(R) - 14` で決める。f32 の半径の正確なビットとの比較ではない。これは v2 とは意図して異なる。半径が `64 * 2^n` のすぐ上（例: 64.001 は v2 で k=1、v3 で k=0。128.001 は v2 で 2、v3 で 1）では v2 と倍率が違ってよく、v2 のハッシュはこの経路を通らないので変わらない。ぼかしの部分木の `RoiQ` の `coarsen` は、テキスト（#31）を含め、この定義に揃える。2 の冪の前後のパリティ（`R ∈ {2^n·2^14 - 1, 2^n·2^14, 2^n·2^14 + 1}` に当たる半径。64.001、128.001 を含む）を `blur_roi_q_matches_f32_roi` に足し、v2 と違う組は「v3 の定義による差」として表明する。

**`rho <= 64` の証明。** 実数の半径を `rho = R / 2^(l + 8)` とする。ぼかしの `stage` は次の三か所からしか呼ばれない。

1. `stack` の末尾のぼかし。`l = L >= clog2(R) - 14` なので `rho = R / 2^(L + 8) <= 2^clog2(R) / 2^(clog2(R) - 6) = 64`。
2. `path_coverage`。feather の `R` で同じ `L` を作るので、同じく `rho <= 64`。`effects` に渡す効果は一つで、その出力 ROI は `sample` そのものである。
3. `children` の `effects`。半径が 0 より大きいぼかしがあれば `stack` を通るので、ここに来るぼかしは `R == 0` の素通しだけである。v3 の判定は `radius > 0.0` ではなく `radius_q8 > 0` にする。`radius = 1e-9` のように f32 で正、`R` で 0 のぼかしは、v3 では背面のハローを作らない。

したがって `r <= ceil(64) = 64`、`rho8 <= 16384` である。ホストは v3 で `r > 64` なら `RenderError::InvalidArgument` を返す（到達しないことを上で示した。v2 のように 64 タップで黙って切らない）。

**レベルの検査。** レベルの範囲は、すべての呼び出し元の不変条件ではない。`Renderer::render` は表示のレベルを切り（`renderer.rs` の `min_level` と `clamp(min_level, 12.0)`）、wasm の `read_tile_level`（`crates/alto-wasm/src/lib.rs`）は `-6..=12` を検査するが、native の公開の `Renderer::read_tile` と `read_tiles` は `(i32, i32, i32)` の鍵をそのまま受け、`copy_display_tile` → `compose` → `compose_m5` が `key.0` を使う。したがって v3 のぼかしの整数の関数（`blur_taps`、整数の `coarsen`、拡大の準備）は、どのシフトや 2 の冪よりも先に、`RoiQ` のレベルが `-6..=12` であることを検査し、外なら `RenderError::InvalidArgument` を返す。`r > 64` と `m > 7` の検査はその後で、溢れるシフトの前に来ることはない。範囲の中では `R << (-l) <= 2^20`、`2^l <= 2^12`、`256 * 2^l <= 2^20` である。#31 のテキストは `L >= -16` まで描くが、v3 ではレベル −7 から −16 のぼかしはエラーになる（「未解決の問題」の 11）。

**拡大の倍率差。** `m = L - level` について、上の検査の `level >= -6` と `clog2(R) <= clog2(16384) = 14`、すなわち `L <= max(level, 0)` から、粗くするのは `level < 0` のときだけで、`m <= 6` である。下の拡大は `m <= 7` を要る。ホストは `m > 7` なら `RenderError::InvalidArgument` を返す。

## 重みの生成

ホストが `(r, rho')` から、半分の重み `w[0..=r]`（Q16、和は正確に 2^16）を作る。浮動小数、`exp`、libm は使わない。全部が非負の整数で、除算は `div_r(n, d) = (n + d / 2) / d`（`d > 0`、`n >= 0`、`/` は切り捨て）である。これは `div_q_i64` の非負の側そのものである。

**1. 指数の引数。** `ONE = 2^32` とする。`i = 0..=r` について、

```text
t_i = 9 * 65536 * i^2 / (2 * rho'^2)               // = i^2 / (2 σ^2)。有理数
y_i = div_r(4608 * i^2 * 2^32, rho'^2)             // = t_i / 64 の Q32
```

`4608 = 9 * 65536 / (2 * 64)` である。

**2. `e^(-y)` の Horner。** 次数 6 の Taylor 多項式を、Q32 の Horner で評価する。

```text
h = ONE
for k = 6, 5, 4, 3, 2, 1:
    h = ONE - div_r(y_i * h, k * ONE)
```

`1 - y(1 - y/2(1 - y/3(1 - y/4(1 - y/5(1 - y/6)))))` である。下の「重みの上限」で `y_i <= 0.28125 * ONE` を示す。そのとき `div_r(y_i * h, k * ONE) <= ONE`（`y_i * h / (k * ONE) <= 0.28125 * ONE` で、丸めても `ONE` を超えない）なので、`h` はいつも 0..=ONE である。

**3. 64 乗。** 6 回二乗する。

```text
repeat 6 times:  h = (h * h + 2^31) >> 32          // div_r(h^2, 2^32)
g_i = h                                             // e^(-t_i) の Q32
```

`h * h` は `h = 2^32` のとき 2^64 で u64 に入らない。Rust は u128、JS は BigInt で計算する。`i = 0` では `y_0 = 0`、Horner も二乗も `ONE` を保ち、`g_0 = 2^32` である。

**4. 正規化。** 尾の和を一度だけ丸める。

```text
S_k = g_k + g_(k+1) + ... + g_r        (k = 1..=r)、S_(r+1) = 0
G   = g_0 + 2 * S_1
P_k = div_r(65536 * S_k, G)            (k = 1..=r)、P_(r+1) = 0
w[k] = P_k - P_(k+1)                   (k = 1..=r)
w[0] = 65536 - 2 * P_1
```

性質は四つある。

- **和は正確に 2^16。** `w[0] + 2 * (w[1] + ... + w[r]) = 65536 - 2 P_1 + 2 (P_1 - P_(r+1)) = 65536`。畳み込みの重みの和は `w[0] + 2 Σ w[k]` である。
- **非負。** `S_k` は `k` について非増加で、`div_r` は単調なので `P_k >= P_(k+1)`、`w[k] >= 0`。`2 S_1 <= G` から `65536 S_1 / G <= 32768`、丸めても `P_1 <= 32768`、`w[0] >= 0`。
- **各重みの誤差は 1 未満。** `w[k]` は二つの丸めの差なので、実数の `65536 g_k / G` から 1 未満しかずれない。`w[0]` も同じである。重みを一つずつ丸めて残りを `w[0]` に寄せると、`w[0]` が最大 `r` だけずれ、`r = 64` で中心が隣より小さくなる（`w[0] = 1222 < w[1] = 1227`）。尾の和の丸めはそれを起こさない。
- **単調。** `w[0] >= w[1] >= ... >= w[r]`。証明はしない。下の網羅テストが、到達し得るすべての `(r, rho8)` で表明する。

**テストのベクトル。** 三実装の単体テストは次の値を表明する。

| `(r, rho')` | `G` | `w[0..=r]` |
| --- | --- | --- |
| `(1, 256)` | 4_390_392_850 | `[64112, 712]` |
| `(2, 256)` | 4_390_392_980 | `[64112, 712, 0]` |
| `(8, 2048)` | 28_670_413_944 | `[9818, 9151, 7411, 5214, 3187, 1693, 781, 313, 109]` |
| `(12, 3072)` | 42_989_068_934 | `[6548, 6346, 5778, 4943, 3971, 2998, 2125, 1416, 886, 521, 288, 149, 73]` |
| `(64, 16384)` | 229_098_771_066 | 先頭 `[1228, 1228, 1223, 1216]`、末尾 `[21, 18, 15, 14]` |

`(8, 2048)` の `g` は `[4294967296, 4003349699, 3242011378, 2281039922, 1394371724, 740545240, 341705490, 136987094, 47712777]` である。`(2, 256)` は、`r` が `rho8 / 256` より一つ大きい組である。たとえばレベル 2 で `R = 1025`（`radius = 4.00390625`）は `rho8 = div_r(1025, 4) = 256`、`r = ceil_div(1025, 1024) = 2` になる。尾の重みは 0 になり得て、それで正しい。

精度は決定性の論点ではないが、記録しておく。到達し得るすべての `(r, rho8)` と `i` で、`g_i / 2^32` と実数の `exp(-t_i)` の差は `1.9e-8` 未満だった（設計時に Python の整数で計算した。実装 PR の Rust のテストで再確認する。表明はしない）。

**WGSL への渡し方。** `w[k]` はすべて 65536 未満である（`r >= 1` で `w[0] <= 64112`。網羅テストが表明する）。二つずつ u32 に詰め、`Params` の `curve`（64 個の 4 バイト）に入れる。

```text
word(j) = w[2j] | (w[2j + 1] << 16)       // j = 0..=32。w[k] は k > r で 0
curve_u32[j] = word(j)                     // curve_u32 = bytemuck::cast_slice_mut::<f32, u32>(&mut p.curve)
```

Rust は `curve` を u32 の列として書き、f32 の値を一度も作らない。(c) のシェーダは、この 256 バイトを `array<vec4u, 16>` として宣言する。f32 として読んでから `bitcast` しない。小さい整数の f32 のビットは非正規化数で、読み込みで 0 に潰す実装があり得る（main の color-adjust の v3 は `bitcast<vec4i>(c.settings)` で読み、3 バックエンドで一致しているが、v3 のぼかしはそれに依らない）。重みは `(curve[j / 4][j % 4] >> (16 * (k % 2))) & 0xffff`、`j = k / 2` で引く。

## 二つのパス

v3 のぼかしの一回の `stage` は、v2 と同じ二つのディスパッチである。**水平が先、垂直が後**で、`middle` は `roi.expand(0, r)`（`RoiQ`）である。新しいモード 24 を (c) のシェーダに足す。

**uniform。** (c) のシェーダは `settings` を `vec4i`、`info` を `vec4u` として宣言する。Rust は `bytemuck::cast_slice_mut` で整数を書く（`curve` と同じく、f32 の値を作らない）。

| 欄 | 中身 |
| --- | --- |
| `info.x` | 24 |
| `info.w` | 軸。0 が水平、1 が垂直 |
| `settings.x` | `r`（1..=64） |
| `settings.y`、`settings.z` | 出力の画素 (0, 0) に対応する入力の画素 `(dx, dy)` |
| `sizes.xy` | 入力の `w`、`h`（今の `dispatch` のまま） |
| `curve` | 上の「WGSL への渡し方」の 33 語 |

`(dx, dy)` はホストが `RoiQ` の差で作る。水平は `dx = middle.ox - input.ox`、`dy = middle.oy - input.oy`、垂直は `dx = out.ox - middle.ox`、`dy = out.oy - middle.oy` である。三つの `RoiQ` は同じレベルでなければならず、ホストはそうでなければ、または差が i32 に入らなければ `RenderError::InvalidArgument` を返す。`stack` から来るとき、入力は `input_roi` の `sample.expand(r, r)` なので、水平は `(r, 0)`、垂直は `(0, r)` である。

**画素の式。** 出力の画素 `p = vec2i(id.xy)` について、四つのチャネルとも同じ式である。

```text
k = p + (dx, dy)
acc = 0                                             // u32
for i = -r ..= r:
    q = k + (axis == 0 ? (i, 0) : (0, i))
    v = q が [0, sizes.x) × [0, sizes.y) の中なら textureLoad(a, q, 0) の Q14、外なら 0
    acc += w[|i|] * v
out = (acc + 32768) >> 16                           // div_r(acc, 65536)
```

結びは `acc ≡ 32768 (mod 65536)` のときで、上へ行く（#24 の非負の丸め）。和は整数で、どの部分和も最終の和以下（項が非負）なので、足す順は結果に影響しない。v2 の f32 の `sum` と違い、積算の順は規定しなくてよい。

**パスの順は規定する。** `middle` で一度丸めるので、水平と垂直を入れ替えると結果が変わり得る。例：`(r, rho') = (1, 256)`（`w = [64112, 712]`）、3×3 の入力 `[[13576, 5669, 12029], [12278, 2833, 14383], [3536, 5364, 12886]]` の中心は、水平が先で 3117、垂直が先で 3116 である。v3 は v2 と同じ水平が先だけを使う。この例は単体テスト `blur_pass_order_is_horizontal_first` に置く。

**端。** 入力の外は 0（透明）で、v2 の `load_a` と同じ意味である。クランプも鏡映もしない。レイヤーの外は透明なので、それが正しいぼかしである。`input_roi` が入力を `r` だけ広げているので、`stack` から来るパスでは 0 の分岐は一度も通らない。水平では、出力の列 `x` は入力の列 `x + r + i`（`i = -r..=r`）、すなわち `[x, x + 2r]` を読み、入力の幅は `w + 2r` なので中にいる。垂直も同じである。したがって出力の画素は、ROI に依らず「レイヤーを無限の平面として、透明な外側ごとぼかしたもの」の切り出しである。タイルの継ぎ目でも同じ画素になる。

**プレマルチプライ。** 四つのチャネルに同じ重みを掛ける。アルファ 0 の画素はプレマルチプライの RGB も 0 なので、透明な画素から色がにじまない。ストレート化はしない。性質は二つある。

- **範囲。** `w[0] + 2 Σ w[k] = 65536` で、`v <= 16384` なので `acc <= 16384 * 65536 = 2^30`、`out <= (2^30 + 32768) >> 16 = 16384`。
- **`rgb <= a`。** 各画素で `v_rgb <= v_a` なら、重みが非負なので `acc_rgb <= acc_a`、`div_r` は単調なので `out_rgb <= out_a`。`min(rgb, a)` は入れない。二つ目のパスも同じである。

## 粗い倍率からの拡大

`stack` と `path_coverage` は、粗くしたとき（`m >= 1`）に `crop` で `desired` へ戻す。v2 のモード 8 の `a_linear` を、新しいモード 25 で整数にする。`m == 0` で ROI が違う `crop` も同じモードで行う（下で示すとおり、`m == 0` は正確なコピーになる）。ROI が同じなら、今と同じく素通しである。

**位置。** 出力 `desired`（レベル `l`、原点 `ox`）、入力 `sample`（レベル `L = l + m`、原点 `OX`）について、

```text
D = ox - OX * 2^m                           // 出力の画素単位。ホストが i64 で作り、i32 に入れる
U = 2 * (p + D) + 1 - 2^m                   // 入力の画素中心からの位置。単位は 1/2^(m+1) 画素
j = U >> (m + 1)                             // 床。U >= 0 なので算術シフトの問題はない
f = (U & (2^(m+1) - 1)) << (7 - m)           // Q8 の端数。0..=255
```

`p` は出力の画素 `vec2i(id.xy)` で、x と y を別々に計算する。導出は v2 の `a_linear` そのものである。v2 は `q = (p_doc - region_a.xy) / region_a.z - 0.5` で、`p_doc = (ox + p + 1/2) * 2^l`、`region_a.xy = OX * 2^L` を入れると `q = (D + p + 1/2) / 2^m - 1/2 = U / 2^(m+1)` になる。位置は v2 と同じ有理数で、端数の分母 `2^(m+1)` は `m <= 7` なら 256 を割るので、Q8 の `f` は正確である（丸めない）。

`D` の範囲。`OX = floor_div(ox, 2^m) - 1` なので、`D = ox - floor_div(ox, 2^m) * 2^m + 2^m` は `[2^m, 2^(m+1))` にあり、`U >= 2^(m+1) + 1 - 2^m > 0` である。`m == 0` の `crop`（ROI の切り出し）では `D` は出力の入力の中での位置で、ホストは `D >= 0` を表明する。

**4 タップ。** `a = v(j)`、`b = v(j + (1, 0))`、`d = v(j + (0, 1))`、`e = v(j + (1, 1))`（入力の外は 0）、`fx, fy` は上の `f` の x と y である。

```text
ab  = a * (256 - fx) + b * fx
de  = d * (256 - fx) + e * fx
out = (ab * (256 - fy) + de * fy + 32768) >> 16
```

`bilinear_byte`（`sample.rs`）、`channel_byte`（`composite.wgsl`）、`bilinearByte`（`cpu-composite.js`）と同じ形、同じ丸めで、値が Q14 になっただけである。

- `m == 0` なら `U = 2(p + D)` で `f = 0`、`out = (a * 65536 + 32768) >> 16 = a`。正確なコピーである。
- 重みの和は 65536 で、`out <= 16384`、`rgb <= a` は二つのパスと同じ理由で保たれる。
- **タップはいつも入力の中にある。** `coarsen` の `w' = ceil_div(ox + w - OX * 2^m, 2^m) + 1` について、出力の画素 `p <= w - 1` の右のタップは `j + 1 <= floor((D + w - 1/2) / 2^m - 1/2) + 1 <= ceil((D + w) / 2^m) = w' - 1` である。左のタップは `D >= 2^m` から `j >= 0` である。したがって 0 の分岐は通らず、拡大も ROI に依らない。粗い格子は `OX = floor_div(ox, 2^m) - 1` で文書の同じ格子に揃っている（v2 の `coarsen` の "Anchor every ROI to the same document grid" と同じ）。

**uniform。** `info.x = 25`、`settings = (Dx, Dy, m, 0)`、`sizes.xy` は入力の大きさである。

## 溢れの上限

上限は解析で、標本ではない。すべての項は 0 以上である。

| 項 | 上限 | 入る型 |
| --- | --- | --- |
| `R` のビットの復号 `m + 2^(s-1)` | `2^53 + 2^52` | u64 |
| `R` | 16384（文書の検証） | u32 |
| `R << (-l)`（`l >= -6`） | `2^20` | u32 |
| `rho8`、`rho'` | 16384（上の証明） | u32 |
| `rho'^2` | `2^28` | u32、i64 |
| `y` の分子 `4608 * i^2 * 2^32` | `4608 * 4096 * 2^32 = 81_064_793_292_668_928` | u64、i64 |
| `y_i` | `1_207_959_552`（`0.28125 * 2^32`） | u64 |
| Horner の `y * h` | `1_207_959_552 * 2^32 = 5_188_146_770_730_811_392` | u64、i64（`2^63 - 1 = 9_223_372_036_854_775_807` 未満） |
| Horner の除数 `k * ONE` とその半分 | `6 * 2^32` | u64 |
| 二乗の `h * h + 2^31` | `2^64 + 2^31` | u128、BigInt。u64 には入らない |
| `G` | `(2 * 64 + 1) * 2^32 = 554_050_781_184` | u64 |
| `65536 * S_k` | `2^16 * 64 * 2^32 = 2^54` | u64 |
| パスの `acc + 32768` | `2^30 + 32768` | u32、i32 |
| 拡大の `ab * (256 - fy) + de * fy + 32768` | `16384 * 65536 + 32768` | u32、i32 |
| 拡大の `U` | `2 * (2^(m+1) + w) + 1`、`w <= max_texture_dimension_2d` | i32 |

**`y_i` の上限の証明。** `y_i = 4608 i^2 / rho'^2`、`i <= r`。実数の半径を `rho`（`0 < rho <= 64`）とすると `r = ceil(rho)`、`rho8 >= 256 rho - 1/2` である。

- `rho <= 1`：`r = 1`、`rho8 <= 256` なので `rho' = 256`。`y <= 4608 / 65536 = 0.0703125`。
- `1 < rho <= 2`：`r = 2`、`rho' >= 256`。`y <= 4608 * 4 / 65536 = 0.28125`。`rho' = 256`（`rho < 1 + 1/512`）で等号。
- `rho > 2`：`r < rho + 1`、`rho' >= 256 (rho - 1/512)`。`(256 r / rho')^2 < ((rho + 1) / (rho - 1/512))^2 < (3 / (2 - 1/512))^2 < 2.26` で、`y < 0.0703125 * 2.26 < 0.16`。

WGSL に `Wide` は要らない。積和はすべて u32 に入る。`Wide` が要るほど大きいのはホストの重みの生成だけで、Rust は u64 / u128、JS は BigInt である。

## 溢れの検査

テスト名と場所は次のとおりである。`color_adjust_q14_mix_does_not_overflow` と同じく、`checked_mul` / `checked_add` が `None` を返すか、上の表の上限を超えたら失敗する。

- **`blur_kernel_q16_is_exact`**（`renderer.rs` のテスト、新規）。到達し得る `(r, rho8)` の組をすべて尽くす。組は「`ceil(rho) = r` かつ `floor(256 rho + 1/2) = rho8` となる実数 `rho ∈ (0, 64]` がある」もので、`rho8 ∈ 0..=16384`、`r ∈ 1..=64` から区間の交わりで数え、16_448 組であることも表明する。各組で、`y_i <= 1_207_959_552`、Horner の各段で `0 <= h <= ONE`、`g_0 = 2^32`、`G <= 554_050_781_184`、`w[0] + 2 Σ w[k] = 65536`、`0 <= w[k] < 65536`、`w` が非増加、を表明する。上の「テストのベクトル」の値も表明する。
- **`blur_radius_q8_matches_rational`**（`m5/mod.rs` のテスト、新規）。`k / 512`（`k = 0..=32768`、偶数番目は正確な値、奇数番目は結び）、`64`、`-0.0`、最小の正規化数と非正規化数で、f64 と f32 の両方の復号が実数の `floor(x * 256 + 1/2)` と一致すること。期待値は整数の有理数で計算する。
- **`blur_roi_q_matches_f32_roi`**（`m5.rs` のテスト、新規）。レベル `-6..=12`、`|tx|, |ty| <= 2^15`、`R ∈ {1, 2, 255, 256, 257, 4096, 8191, 8192, 8193, 16383, 16384}` で、`RoiQ` の `expand` と `coarsen` と `blur_taps` が、v2 の `Roi` の f32 の結果と同じ ROI になること。その範囲では途中の端点も `2^24` 以下で v2 の f32 は正確なので、これは意味が変わらないことの検査である。上の反例（`ox = 16_776_959`、`w = 258`）で v3 が `w' = 132` を返すことも表明する。v2 の `log2` / `powf` が 2 の冪を返さないホストでは、その組を失敗にせず記録する（それが「今日のずれ」の 5 である）。
- **`blur_rejects_out_of_range_levels`**（`m5.rs` のテスト、新規）。レベル `{-7, -40, 13, i32::MIN, i32::MAX}` の `RoiQ` で、`blur_taps` と整数の `coarsen` が、シフトの前に `RenderError::InvalidArgument` を返すこと（デバッグビルドで溢れの panic が起きないこと）。
- **`blur_q14_does_not_overflow`**（`cpu_composite.rs`、新規）。到達し得るすべての重みで、入力が全部 16384 なら `acc = 2^30` ちょうど、出力 16384。入力の画素の RGB とアルファを #24 の `E = {0, 1, 4095, 4096, 4097, 8191, 8192, 8193, 16383, 16384}` の `rgb <= a` の組（55 組）から位置ごとに循環させて取り、`out_rgb <= out_a <= 16384`。拡大は `m ∈ 0..=7`、`fx, fy ∈ 0..=255` の全数で四隅が全部 16384 なら 16384、四隅を `E` の直積と `fx, fy ∈ {0, 1, 127, 128, 255}` で取って `rgb <= a` と 16384 以下。
- **`blur_pass_order_is_horizontal_first`**（`cpu_composite.rs`、新規）。上の 3×3 の例。
- **`blur_window_crop`**（`cpu_composite.rs`、新規）。大きな ROI でぼかした結果と、それを 2×2 に割った ROI でぼかした結果の切り出しが全バイト一致する。粗くしない場合と、`m = 1, 4, 6` の場合を含む。

## 一致の期待

一致は全バイト一致である。バックエンドは #24 と同じ三つである。Metal は native ジョブ（`Apple Paravirtual device`、実機は未検証）、lavapipe は gpu-parity ジョブ、SwiftShader は wasm のブラウザジョブ（アダプタの architecture に `swiftshader`）。ブラウザは color-adjust と同じく `tests/fixtures/render-v3-empty.alto` を開いて v3 の文書にし、そこへラスターと効果を足す。

一致を期待する理由は一つである。文書の値はビットから整数の `R` になり、重み、ROI、オフセットはすべてホストの整数で、Rust の一つのソースが native と wasm で同じ値を作る。GPU は (c) の `rgba16uint` を `textureLoad` で読み、u32 の積和と 2 の冪での丸めだけを行い、上の表で溢れがないことを示した。ぼかしの段に浮動小数はない。

| ケース | 中身 | WGSL = Rust = JS | Metal | lavapipe | SwiftShader | 扱い |
| --- | --- | --- | --- | --- | --- | --- |
| `blur-v3-square` | 既存の xfail と同じ `square()`（透明の上に不透明の赤、80..180）、半径 8、レベル 0 | 一致 | 一致 | 一致 | 一致 | enforce |
| `blur-v3-gradient` | 入力は `colorAdjustGradient`、半径 12 | 一致 | 一致 | 一致 | 一致 | enforce。アルファ 0、1、128、254 の画素でプレマルチプライと `rgb <= a` を踏む |
| `blur-v3-radii` | `square()` を半径 `{0.001, 1/512, 1, 63.99, 64}` のレベル 0 と、半径 `4.00390625` のレベル 2 で | 一致 | 一致 | 一致 | 一致 | enforce。0.001 は `R = 0` の素通し、1/512 は `R = 1`（結びが上へ）、64 は 129 タップ、レベル 2 は `(r, rho') = (2, 256)` |
| `blur-v3-seam` | 不透明の矩形 220..290、半径 12、タイル (0,0)、(1,0)、(0,1)、(1,1) | 一致 | 一致 | 一致 | 一致 | enforce。参照は文書全体を一枚で計算して切り出す |
| `blur-v3-stack` | 半径 6 と 9 の直列 | 一致 | 一致 | 一致 | 一致 | enforce |
| `blur-v3-level1` | `square()`、半径 8、レベル 1（`rho = 4`） | 一致 | 一致 | 一致 | 一致 | enforce |
| `blur-v3-zoom` | `square()` をレベル −2 の半径 32（`m = 1`）、レベル −6 の半径 12（`m = 4`）と 64（`m = 6`） | 一致 | 一致 | 一致 | 一致 | enforce。(c) の負のレベルのラスターの入口が整数で一致してから足す |
| 選択つきのぼかし | `selection` を持つぼかし | 一致 | 一致 | 一致 | 一致 | enforce。#26 のモード 5 の実装 (d) が入ってから |
| 背面ぼかし | 不透明な背面の上の調整レイヤーにぼかし | 一致 | 一致 | 一致 | 一致 | enforce。#24 の M5 のブレンド（空でない背面）が入ってから |
| グループのぼかし | 半透明の子を持つグループにぼかし | 一致 | 一致 | 一致 | 一致 | enforce。同上 |
| ベクターのぼかし、feather | ベクターレイヤーのぼかし、feather つきのベクターマスク | 期待しない | — | — | — | measured-not-enforced。入力のカバレッジが vector-fill の v3 待ち |
| テキストのぼかし | テキストにぼかし | 期待しない | — | — | — | measured-not-enforced。#31 の v3 待ち |
| v2 の `gaussian-blur` | `createDocument` のまま | — | — | — | — | xfail のまま |
| v2 の計測 | 同じ `square()` を v2 で、3 バックエンドの差の画素数。半径 64 をレベル −1 と −6 で（`coarsen` の `log2`） | — | 計測 | 計測 | 計測 | measured-not-enforced |

Rust の参照は、ラスターのタイルとぼかしの効果をシーンに直接置ける（`plain_scene_gaps_are_explicit` が `REffectKind::GaussianBlur { radius: 8.0 }` を置くのと同じ組み方。v3 では `radius_q8` も置く）。参照の関数は `blur_pass_q14`、`resample_q14`、`render_blur_v3`（`cpu_composite.rs`、テストのみ、新規）で、重みと ROI は本番の `blur_kernel_q16`、`RoiQ`、`blur_taps` を使う。

JS の参照は `blurRadiusQ8`、`blurKernelQ16`、`blurPassQ14`、`resampleQ14`、`renderBlurV3`（`cpu-composite.js`、新規）で、すべて独立に書く。整数は、重みの生成が BigInt、画素の積和が `Number`（2^31 未満なので正確）である。入力のラスターは、恒等の配置で、レベル `l` の画素 `i` が読むテクセルを整数で `floor_div(2 (ox + i) + 1, 2^(1 - l))`（`l <= 0`）、`(ox + i) * 2^l + floor_div(2^l, 2)`（`l >= 1`）とし、`premulByteQ14` で Q14 にする。これは v2 の `sparse` の最近傍の `floor` と同じ位置である。参照は文書全体を無限の平面として計算し、タイルを切り出す。上の「端」と「タップはいつも入力の中にある」により、GPU の ROI の連鎖と同じ画素になる。

ケースの名前は `blur-v3-` で始める。

## enforce と measure

enforce するのは、構成上一致し、3 バックエンドの CI で全バイト一致したケースだけである。上の表で enforce と書いたものは、実装 PR で一致を確かめてから通常のケースとして入れる。期待どおりに一致しなかったら、measure に落とさない。設計に戻る（#24 のアトラス読み取りと同じ扱い）。前提（(c)、(d)、#24 の M5 のブレンド）が入っていないケースは、入るまで足さない。`blur-v3-` のケースの入力はすべてラスターなので、(c) のラスターの入口が `RoiQ` から整数で位置を作るようになっていることも前提である。

measure するのは、v2 の `gaussian-blur` と、ぼかしの入力がまだ v3 でない経路（ベクター、テキスト）である。`measureRow` と `formatMeasure` で `measure: true, enforced: false` の行にし、失敗にしない。

## v1 / v2 を動かさない

- `effects.wgsl` のモード 2 とモード 8、`load_a`、`a_at`、`a_linear` の文は一文字も変えない。v3 は (c) のシェーダの新しいモード 24 と 25 を使う。0..=19 と 6 は再利用しない（#26）。マスクの実装 (d) は 24 と 25 を避ける。
- `Roi`、`Roi::expand`、`Roi::coarsen`、`input_roi`、`effects`、`stage`、`stack`、`crop`、`path_coverage`、`children` の v2 の文は変えない。v3 は `self.scene.render_version >= alto_core::RENDER_VERSION_EXACT` の分岐で、`RoiQ`、`blur_taps`、`radius_q8` を使う。`blend`、`stage`、`raw_node` と同じ形である。
- `REffectKind::GaussianBlur` に足す `radius_q8` は、v2 では読まない。`radius: f32` とその作り方（`render_effect` の `n as f32`）は変えない。シーンの `PartialEq`（`REffectKind` の derive）は細かくなる。今は f32 の `radius` だけを比べるので、f32 に縮めると同じになる二つの文書の値（たとえば `1/512 - 2^-40` と `1/512 + 2^-40` はどちらも f32 の `0.001953125`）が等しい。`radius_q8` は f64 から直接作るので、この二つは 0 と 1 になり、等しくなくなる。これは意図した変更である。v3 ではこの二つの画素は違うので、等しいとみなしてはいけない。v2 の画素は f32 の `radius` だけで決まるので、等しさが細かくなっても v2 の画素は変わらない。今の無効化はこの等しさに依らない。`scene_damage`（`crates/alto-scene/src/damage.rs`）は、効果を持つシーンでは `plain_scene` が偽なので、比べる前に `Damage::All` を返す。等しさが変わることは単体テスト `blur_radius_q8_refines_scene_equality`（`m5/mod.rs`、新規）で表明する。
- 文書の形式は変えない。`radius` は今の f64、`feather` は今の f32 で、検証の範囲 `0.0..=64.0` も変えない。
- `tests/e2e/m5-tests.js` の `weights` と `intervalBlur` は v2 の文書の許容誤差つきの検査として残す。
- `tests/parity/render-v1.txt` と `tests/parity/cpu-reference.txt` の既存行は一文字も変えない。v3 の行を足すだけである。今の `existing_v1_and_v2_hashes_are_unchanged`（`cpu_composite.rs`）は、`cpu-reference-v2-before-alpha.txt` より後の行に `-alpha-measure ` を含むことを要求するので、`blur-v3-` の行はそのままでは落ちる。実装順の 6 で、後ろに足せる行に「名前が `blur-v3-` で始まる」を加える。#31 の実装が先に `text-v3-` を加えていれば、同じ条件に並べる。前置の一致と v1 の全体の一致の検査は残す。v2 のぼかしにはロック済みのハッシュがない（xfail）が、出荷済みの v2 の文書の画素は、文を変えないことで守る。
- `plain_scene` と `gap` は変えない。ぼかしのあるシーンは v3 でも合成パスに乗らない。
- ゲートは今のまま使う。Rust は `allow_unstable_render_v3`（`crates/alto-engine/src/unstable_render.rs`）で、デバッグビルド、コンパイル時の許可（このクレートのテストかフィーチャ `unstable-render-v3`）、実行時の要求の三つがすべて要る。JS は `renderVersionUpgrade`（`packages/alto-sdk/src/render-version.ts`）で、`{ unstableRenderV3: true }` を渡しても、ビルドの許可（既定は出荷定数 `UNSTABLE_RENDER_V3` で false）がなければ `rejected` になる。
- シェーダが変わるのでリリース WASM のバイナリのハッシュとサイズは動く。PR #29（main の `ffdc1bf`）以降、`wasm/SHA256SUMS` と `verification/build.json` は各 PR では更新しない。`RELEASING.md` のとおり、タグの前に main の `wasm-hashes-main` から一度だけ更新する。描画のハッシュとは独立である。

## 実装順

前提は #26 の「実装順」の (b)（#24 の設計を実装する v3 composite、PR #27）と (c)（M5 の Q14 中間）である。1 から 3 と 5 は前提に依らず、どこからも呼ばないので画素を変えない。各項目は main から切った一つの PR である。

1. **整数の核。** `blur_radius_q8_f64` と `blur_radius_q8_f32`（`m5/mod.rs`）、`blur_kernel_q16`（`renderer.rs`）、`RoiQ` と `blur_taps` と整数の `coarsen`（`m5.rs`）。単体テスト `blur_kernel_q16_is_exact`、`blur_radius_q8_matches_rational`、`blur_roi_q_matches_f32_roi`、`blur_rejects_out_of_range_levels`。`RoiQ` は #31 と同じ型で、先に入った PR が定義し、後の PR はそれを使う。
2. **シーンの値。** `REffectKind::GaussianBlur` の `radius_q8` と、`render_effect` と `path_coverage` での設定。`blur_radius_q8_refines_scene_equality`。v2 の画素は変わらない。
3. **Rust の参照。** `cpu_composite.rs` の `blur_pass_q14`、`resample_q14`、`render_blur_v3` と、`blur_q14_does_not_overflow`、`blur_pass_order_is_horizontal_first`、`blur_window_crop`。
4. **WGSL とホスト。** (c) のシェーダのモード 24 と 25、uniform の整数の宣言、`stage`、`stack`、`effects`、`input_roi`、`crop`、`path_coverage`、`children` の v3 の分岐。native の 2 バックエンドで、上の enforce のケースを Rust の参照と比べる。(c) のあと。
5. **JS の参照。** `blurRadiusQ8`、`blurRadiusQ8F32`、`blurKernelQ16`、`blurPassQ14`、`resampleQ14`、`renderBlurV3`。ブラウザの実行で、上の「テストのベクトル」の重みを自己検査する（違えば失敗）。
6. **固定。** enforce のケースを 3 バックエンドで流し、一致を記録してから `blur-v3-` の行を `cpu-reference.txt` に足す。同じ PR で `existing_v1_and_v2_hashes_are_unchanged` の追記の規則を広げ、`gaussian-blur` の xfail の隣に v3 のケースを置く（v2 の xfail は残す）。
7. **measure の行。** v2 の計測と、ベクターとテキストのぼかし。選択つき、背面、グループのケースは、それぞれの前提が入った PR で enforce として足す。

## 未解決の問題

1. **入力の段の `RoiQ`。** ぼかしの段は `RoiQ` だけを使う。その入力を描く段のうち、テキストは #31 が `RoiQ` で定義した。ラスターの入口（今は f32 の `sparse`）とベクターは、(c)、#26、vector-fill で `RoiQ` から整数の位置を作る必要がある。#24 の後回しの宿題（`composite-q14.wgsl` の軸単位の f32 座標）と同じ種類の問題である。それが入るまで、その入力のぼかしは enforce しない。
2. **`middle` の形式。** この文書は `rgba16uint` に丸めて置き、パスの順を規定した。`rgba32uint` に丸めない和を置けば一回の丸めになり、順にも依らなくなるが、プールの形式が二つになり、WGSL に `Wide` が要る。レビューで逆を選ぶなら、「二つのパス」と溢れの表の二つ目のパスを書き直す。
3. **重みの定数の凍結。** 次数 6、64 乗、Q32、`4608`、尾の和の正規化は v3 の画素の一部である。どれかを変えれば v3 のハッシュが変わる。変えるときは描画バージョンを上げる、と規則にするかは決めていない（#31 の依存の版と同じ論点）。
4. **小さい半径の意味。** `radius < 1/512` は v3 で素通しになる。v2 は、f32 に縮めた半径が正なら、どのレベルでも `r >= 1`、`σ = 0.333333` で僅かにぼかしていた（f32 で 0 に落ちる半径だけが素通し）。製品として問題ないかは決めていない。
5. **大きい半径の費用。** 一画素あたり最大 129 タップを二回である。v2 と同じ数だが、u32 の積和のほうが f32 の `exp` より軽いはずである。計測はしていない。箱ぼかしの反復などの近似は、別の重みになるので v3 に入れない。
6. **uniform の型。** この文書は (c) のシェーダが `settings` と `curve` を整数として宣言すると決めた。main の color-adjust の v3 は `bitcast<vec4i>(c.settings)` で f32 として読んでいる。(c) で同じ宣言に揃えるかは (c) で決める。
7. **v2 の `coarsen` の libm。** 「今日のずれ」の 5 は v2 に残る。v2 の計測の行（半径 64 のレベル −1 と −6）で、native と wasm で実際に倍率が分かれるかを見る。分かれても v2 は直さない。対処は `render.upgrade` である。
8. **feather の意味。** `path_coverage` は feather をそのままぼかしの半径にしている（σ は feather の 1/3）。v3 も同じ意味にした。feather の幅の定義を変えるかは、vector-fill の v3 で決める。
9. **モード番号の調整。** 24 と 25 をここで取った。(d) のマスクと (c) が使う番号は、まだどの文書も固定していない。衝突したら後から入る PR が別の番号を取り、この文書の番号を直す。
10. **古いメモの指し先。** [正確な CPU 参照](2026-10-27-exact-cpu-references.md) の「gaussian-blur」節は、この文書で置き換えた。あの節からこの文書への指し先は、実装 PR で足す。この変更は文書を一つ足すだけで、既存の文書は触らない。
11. **レベルの範囲の食い違い。** v3 のぼかしはレベル `-6..=12` だけを受ける（拡大の Q8 が `m <= 7` を要るため）。#31 のテキストは `L >= -16` まで描く。ぼかしの中のテキストをレベル −7 以下で描く呼び出しは、native の `read_tile` からしか来ないが、そのときはエラーになる。拡大の端数を Q8 より細かくしてレベルを広げるかは決めていない。
12. **`coarsen` の倍率の定義の一本化。** 改訂後の #31 は、粗い倍率の `k` を f32 の `minimum`（文書の半径 / 64）と 2 の冪の比較で決める（その文書ではぼかしが v2 のままだからである）。この文書は `k = clog2(R) - 14` で、Q8 の `R` から決める。半径が 2 の冪 × 64 のすぐ上にあるとき（たとえば `radius = 32 + 2^-18`（f32 で正確）：f32 の比較では `minimum > 1/2` で `k = 0`、`R = 8192` から `k = -1`）、二つは違う。`rho <= 64` の証明はこの文書の定義に依るので、ぼかしの v3 が入るときは、ぼかしの部分木の `RoiQ` の `coarsen` をこの文書の定義に揃える。#31 の文言の改訂は、その実装 PR で行う。
