# v3 整数合成

作成日：2026-10-28（Asia/Tokyo）

状態：設計のみ。この文書は実装しない。描画バージョン 3 は、ここにある整数合成が入り、Metal・lavapipe・SwiftShader で一致するまで不安定なオプトインのままである。描画バージョン 2 の式と、ロック済みハッシュは凍結したままである。

関連：[画像編集](IMAGE-EDITING.md)、[半透明合成の計測](../verification/semi-transparent-composite-parity.md)、[正確な CPU 参照](2026-10-27-exact-cpu-references.md)。PR [#22](https://github.com/NobukiAkiyama/alto-engine/pull/22) は main の `adf70b3` に入っている。

## 前提

The GPU unorm store conversion is allowed error by the API spec, so it cannot be a determinism foundation. v3 does all quantization in its own integer formulas and writes its final tile output only to `rgba8uint` or packed u32. Intermediates may use `rgba16uint` holding Q14 integers. No float intermediate exists anywhere in v3. No path may write to a float or unorm target and let hardware round.

GPU の unorm ストア変換は、API 仕様が誤差を許している。WebGPU / Vulkan の float から `rgba8unorm` への書き込みは、`roundTiesToEven` であるとも、`floor(x + 0.5)` であるとも要求されない。実装がどちらに寄せても仕様違反ではない。したがって、この変換は決定性の基礎にできない。

v3 は量子化をすべて自分の整数式で行う。最終のタイル出力は `rgba8uint` かパック u32。中間値は Q14 の整数で `rgba16uint` を許す。float の中間は v3 のどこにも置かない。float または unorm のターゲットへ合成結果を書き、ハードウェアに丸めさせる経路は一本もない。

計測は [半透明合成の計測](../verification/semi-transparent-composite-parity.md) にある。v2 では、事前量子化の f32 が CPU とビット一致しても、`rgba8unorm` のストアが ±1 になる。P2P ではその ±1 がタイルハッシュの分岐になり、同じ文書がピアによって違う。v2 はその既知の非決定性を持ったまま凍結する。既存文書の対処は `render.upgrade`（`upgrade_render_version` / `upgradeRenderVersion`）で、将来の `render_version` 3 の整数合成へ上げることである。この設計が入るまで、その更新が届く先は main の color-adjust であり、合成そのものは v2 の float `blend` のままである。

## main の Q14 を使う

Q14 の 1.0 は 16384 である。次の名前は main にある。合成もそのまま使う。式の本体はここに写さない。

| 名前 | 場所 | 役割 |
| --- | --- | --- |
| `div_q_i64` | `crates/alto-gpu/src/cpu_composite.rs` の `div_q_i64` | `d > 0` の `(abs(n) + d/2) / d`。符号は結びを 0 から遠い側へ送る |
| `div_q` | `crates/alto-gpu/shaders/effects.wgsl` の `div_q` | 同じ丸めの i32 除算 |
| `wide_div_q14` | 同ファイルの `wide_div_q14` | `Wide` を 16384 で割る。絶対値に 8192 を足してからシフトする。負数の算術シフトは使わない |
| `divQBig` | `tests/e2e/cpu-composite.js` の `divQBig` | 同じ丸めの BigInt 除算 |
| `Wide` | `effects.wgsl` の `struct Wide`（`lo: u32, hi: u32`）。形は `crates/alto-gpu/shaders/composite.wgsl` の `struct Wide` と同じ | i32 に乗らない積。Rust は i64 の `checked_mul` / `checked_add`、JS は BigInt |
| `byte_q14` / `premul_byte_q14` | WGSL `effects.wgsl` と Rust `cpu_composite.rs`。JS は `byteQ14` / `premulByteQ14` | unorm バイトから Q14 へ入る。不透明 255 は 1 のまま |
| 輝度の重み 3483, 11718, 1183 | Rust と JS の `Q14_LUMA`。WGSL は `effects.wgsl` のリテラル | 和は 16384 |
| `color_adjust_q14` | `crates/alto-gpu/src/renderer.rs` の `color_adjust_q14` | 設定値を半整数で 0 から遠い側へ量子化する |
| `color_adjust_q14_mix_does_not_overflow` | `cpu_composite.rs` の同名テスト | `checked_mul` / `checked_add` が溢れたら失敗する |

丸め規則はこれ一つである。結びは 0 から遠い側へ行く。合成の中に `round_ties_even` は置かない。ハードウェアの unorm 丸めも置かない。

color-adjust のストレート化は `effects.wgsl` の `(u32(q) << 14u) / u32(alpha)` で、切り捨てである。合成の除算は、ストレート化を含めて全部 `div_q` / `wide_div_q14` / `div_q_i64` である。切り捨て除算を合成へコピーしない。color-adjust 側の式は変えない。

color-adjust は、段のあいだを f16 で書き、最後を unorm の偶数丸めで解決する。その解決は color-adjust の範囲であり、この合成はそれを入力の float テクスチャとして読まない。両方が入ったあとの受け渡しは、Q14 の整数か `rgba8uint` のバイトである。モード 19 が「背面が正確な 0 で normal のとき f16 をコピーする」のは color-adjust の記述のまま残す。合成経路が整数になったとき、空の背面への normal は Q14 のコピーになる。color-adjust の式はここで再定義しない。

v3 を既定の新規文書にはしない。main の不安定ゲートを維持する。Rust はデバッグビルドで、このクレートのテストかフィーチャ `unstable-render-v3` があるときだけ 3 への更新を受ける。JS は `{ unstableRenderV3: true }` で、出荷定数 `UNSTABLE_RENDER_V3` は false である。既定の新規文書はバージョン 2 のままである。`DocumentMeta::validate` が受ける `render_version` は 1、2、3 だけ、という main の制約も維持する。

## 値の置き場

ストレートの色、プレマルチプライの色、アルファは、いずれも Q14 の 0..=16384 である。アキュムレータはプレマルチプライ Q14 である。

アキュムレータをテクスチャへ書くときは `rgba8uint` に、バイトそのものを書く。ワイドな途中値をバッファへ溢すときは、パックした u32 のストレージである。画素あたり u32 が 2 つあれば、チャネルを 16 ビットで 4 本置ける。0..=16384 は 15 ビットに収まる。`rgba8unorm` へ `vec4<f32>` を `textureStore` しない。`rgba16float` へ合成結果を書かない。グループのスタック（深さ 32）はシェーダのプライベート配列に置き、float テクスチャには書かない。

v2 の出力バインディング `texture_storage_2d_array<rgba8unorm, write>` は v2 のシェーダに残す。v3 のシェーダは別で、出力は `rgba8uint` かパック u32 である。読み戻しもその整数を読む。unorm としてサンプルしない。

タイル出力はプレマルチプライである。`div_q_i64(q * 255, 16384)` の `q` はプレマルチプライ Q14 アキュムレータのそのチャネル（RGB もアルファも同じ）で、0..=255 に切ったバイトを `rgba8uint` に書く。ストレート化してからバイトにはしない。この順だけを Rust、WGSL、JS が使う。

## 座標と重み（v2 整数 4 タップ）

`rotation` と `scale-bilinear` のタップ位置と重みは、f32 の座標計算ではない。v3 で位置と重みを作り直さない。

ホストが逆変換を Q16 に量子化し、シェーダはその整数だけを使う。

- Rust `pack_inverse` / `integer_inverse`（`crates/alto-gpu/src/sample.rs`）。係数は `quantize_coeff`（Q16、i32）、平行移動は `quantize_translation`（Q16、i64）。入らないときだけ `SAMPLE_F32` に戻る。軸平行かつ倍率 1 も f32 の最近傍のままである。`rotation`（0.3 rad）と `scale-bilinear`（倍率 2）はそのどちらでもないので、逆変換が収まる限り整数経路である。
- JS は `quantizeInverse`、`pixelQ8`、`sourceAxis`、`floorFracQ8`、`bilinearByte`、`fetchInteger`（`tests/e2e/cpu-composite.js`）。`sourceAxis` は BigInt である。
- 画素中心は Q8 の整数である。`pixel_q8`（WGSL `composite.wgsl`、Rust `sample.rs` の `pixel_q8`、JS `pixelQ8`）。`origin * 256 + (index * 256 + 128) * scale`。`config.origin_scale` は整数を入れた f32 で、シェーダはすぐに `i32` へ戻す。f32 の行列でタップ位置は出さない。
- ソース位置は `coord_q8`（WGSL）と `source_q8` / `source_axis`（Rust）。Q16 行列 × Q8 中心に、Q16 平行移動を Q24 へシフトして足し、0 から遠い側へ丸めて Q8 にする（`wide_round_shr16`、Rust `round_shr16`、JS `roundShr16`）。
- 重みは Q8 の下位 8 ビット `fx, fy`（0..=255）。`floor_frac_q8` と WGSL の `qx.v - (x << 8)`。
- 4 タップは `channel_byte`（WGSL）と `bilinear_byte`（Rust、JS）。`(acc + 32768) >> 16`。

v3 が変えるのは、そのバイトの読み方である。WGSL の `sample_byte` は `Rgba8Unorm` の `textureLoad` を `round(s * 255)` でバイトに戻している。アトラスの形式は `texture`（`crates/alto-gpu/src/renderer.rs`）の `Rgba8Unorm` である。CPU の `sample_u8` と JS の `sampleU8` はバイト列を直接読む。この float の往復は位置でも重みでもない。位置と重みの式は上のままである。

### アトラスの読み方

アトラスは `Rgba8Unorm` のテクスチャ一枚のままにする。v3 用の二枚目は作らない。

- v3 は `textureLoad` で読み、その場で `unorm_byte`（`u32(clamp(v, 0, 1) * 255 + 0.5)`、`effects.wgsl`）でバイトに戻す。`round(s * 255)` は使わない。`unorm_byte` は #22 の color-adjust モード 18 がすでに使い、3 バックエンドで差 0 だった関数である。
- その後はすべて整数である。`textureSample` もハードウェアのフィルタも使わない。4 タップの補間は上の `channel_byte` が整数で行う。
- 二枚目のアトラス（たとえば `Rgba8Uint`）を並べない理由は三つ。VRAM が倍になる。アップロードが倍になる。内容ハッシュをキーにするタイルキャッシュが二つになる。
- `Rgba8Uint` へ切り替えるのは、どれかのバックエンドで `unorm_byte` の不一致が計測されたときだけである。下の「検証」のアトラス読み取り項目は必須の検査であり、不一致が出たらテストが落ちる。

## ストレートとプレマルチプライ

バイトの入口は main の `premul_byte_q14` と `byte_q14` である。

- 不透明でない色のプレマルチプライは `premul_byte_q14(color, alpha)`。
- ストレートのアルファは `byte_q14(alpha)`。
- ストレートの RGB も `byte_q14`。

不透明度の量子化はシェーダでは行わない。ホスト（Rust と JS）が、文書の f32 を 0..=1 に切ってから `color_adjust_q14` と同じ「0.5 を足して 0 へ向かって切り捨てる」規則で Q14 整数（0..=16384）にし、その整数をドローの uniform として渡す。WGSL が読むのはその整数だけである。WGSL に f32 の不透明度はない。`color_adjust_q14` 本体の −1..=4 は使わない。

不透明度が 1 のとき、`div_q(q * 16384, 16384)` は q そのものなので、バイト入口は `premul_byte_q14` のままである。不透明度が 1 でないときは、その Q14 を一度だけ掛ける。`div_q(premul_byte_q14(color, alpha) * opacity_q, 16384)` と、`div_q(byte_q14(alpha) * opacity_q, 16384)` である。

アキュムレータのストレート化は、アルファが 0 なら 0、そうでなければ `clamp(div_q(premul * 16384, alpha), 0, 16384)` である。color-adjust の切り捨て `(q << 14) / alpha` は使わない。

## グループ

制御は v2 の `walk` と同じである。孤立させるかはホストが決める。条件は文書の f32 に対する `isolated || opacity < 1.0 || mode != normal` で、Q14 に量子化したあとの比較にはしない。1 未満の f32 が Q14 で 16384 になっても、v2 と同じく孤立させる。ホストはその結果を uniform のフラグとして渡す（プッシュとポップを出すかどうか）。シェーダは `opacity < 1.0` を評価しない。量子化した不透明度の整数は、ポップのときのスケールにだけ使う。

孤立するグループは、透明なアキュムレータへ子を合成し、ポップで親へ戻す。ポップは次の順である。

1. 子のプレマルチプライ Q14 を、上の `div_q` でストレート化する。
2. アルファを `div_q(alpha * opacity_q, 16384)` にする。
3. グループのブレンドモードで親へ合成する。

`opacity == 1` かつ normal かつ非孤立のグループはプッシュしない。子は親のアキュムレータへ直接合成する。非表示、または不透明度 0 は、v2 と同じく落とす。

## ブレンド

ブレンド関数の引数はストレート Q14 の 0..=16384 で、結果も同じ範囲に切る。`mul(a, b)` は `div_q(a * b, 16384)` である。以下のモード番号は `blend.wgsl` の `mix_color` と同じである。

三項の和は、float の

```text
(1 - src.a) * dst.rgb + (1 - dst.a) * src.rgb * src.a + dst.a * src.a * b
```

を、共通分母 16384² の一回の `div_q` にする。`dst.rgb` はプレマルチプライ Q14（`dp`）、`src.rgb` と `b` はストレート Q14 である。

```text
num = (16384 - src_a) * dp * 16384
    + (16384 - dst_a) * src_rgb * src_a
    + dst_a * src_a * b
out_rgb = div_q(num, 16384 * 16384)
out_a   = div_q(src_a * 16384 + dst_a * (16384 - src_a), 16384)
```

`b` を 0..=16384 に切ってから足す。項は非負なので、和は 0 以上である。`dp, da, ss, sa, b` がそれぞれ 0..=16384 のとき、分子は `16384³ = 4_398_046_511_104` 以下である。導出は、`dp <= 16384`、`ss <= 16384`、`b <= 16384` を入れると

```text
num <= (16384 - sa) * 16384² + sa * 16384 * (16384 - da) + da * sa * 16384
     = 16384³
```

である。`da * sa * 16384` の項が打ち消し、等号は `sa = 0, dp = 16384` と `sa = ss = 16384, da = 0` で成り立つ。因子を独立に最大にした `3 * 16384³` は、この式では同時に取れない。上の `16384³` が解析的な上限で、i64 と WGSL の `Wide` に入り、i32 には入らない。16384² は u32 に入るので、三つの Q14 の積は「u32 に入る二つの積」を `wide_mul` で三つ目に掛ければ `Wide` に乗る。16384 で割る段は `wide_div_q14` を使う。16384² で割る段は、同じ「絶対値に除数の半分を足してからシフトする」規則で、シフト量は 28 である。Rust は `div_q_i64`、JS は `divQBig` がこの除数を受ける。

`min(out_rgb, out_a)` は入れない。入力が `0 <= dp <= da <= 16384` かつストレートと `b` が 0..=16384 のとき、割り算の前の有理数は `out_rgb <= out_a` である。差は

```text
sa * (16384 - ss) * (16384 - da) + (da - dp) * 16384 * (16384 - sa)
```

まで下から抑えられ、これは 0 以上である。`div_q` は非負の結びを 0 から遠い側（上）へ送る。二つの商がこの丸めで整数になったとき、`out_rgb > out_a` になるには割る前の有理数が `out_rgb > out_a` でなければならない。したがって丸めたあとも `out_rgb <= out_a` である。normal は分母が同じで、プレマルチプライの分子同士が `src_pre <= src_a` と `dst_pre <= dst_a` から大小を保つ。溢れの検査は、端点の全組合せで `out_rgb <= out_a` を失敗条件にする。丸めを変えるとこの証明は成り立たない。

**normal（0）。** ブレンド色はソースそのものである。ストレート化しない。バイト入口のプレマルチプライを、source-over で載せる。

```text
out_rgb = div_q(src_pre * 16384 + dst_pre * (16384 - src_a), 16384)
out_a   = div_q(src_a * 16384 + dst_a * (16384 - src_a), 16384)
```

`src_pre` は上のプレマルチプライ変換の結果である。

**multiply（1）。** `B = mul(dst, src)`。

**screen（2）。** `B = 16384 - mul(16384 - dst, 16384 - src)`。

**overlay（3）。** チャネルごと。`dst < 8192` のとき `div_q(2 * src * dst, 16384)`。そうでないとき `16384 - div_q(2 * (16384 - src) * (16384 - dst), 16384)`。0.5 の閾値は 8192 であり、`byte_q14(127)` は 8192 未満、`byte_q14(128)` は 8192 より大きい。float の `d < 0.5` と同じ側に分かれる。

**darken（4）/ lighten（5）/ add（16）。** `min`、`max`、`min(16384, src + dst)`。除算はない。alpha-measure がこの三つのモードを含むので、同じ Q14 の範囲で定義する。

**color_dodge（6）。** `src >= 16384` なら 16384。そうでなければ `min(16384, div_q(dst * 16384, 16384 - src))`。分母は 1 以上である。float の `max(1 - src, 0.000001)` は要らない。

**color_burn（7）。** `src == 0` なら 0。そうでなければ `16384 - min(16384, div_q((16384 - dst) * 16384, src))`。

**hard_light（8）。** overlay と同じ式で、比較を `src < 8192` にする。

**soft_light（9）。** float の `sqrt` は呼ばない。`d <= 4096`（`dd <= 0.25`。`byte_q14(63) <= 4096 < byte_q14(64)`）のとき、多項式 `16 d³ - 12 d² + 4 d` を i64 で一度に評価し、`div_q(16 d³ - 12 d² * 16384 + 4 d * 16384², 16384²)` とする。`d = 4096` では余りなしで 8192 になる。この分岐の `d` は 0..=4096 なので、`16 * 4096³ = 1_099_511_627_776` が i64 の最大項で、i32 には入らない。

`d > 4096` のとき、`d2` は `d * 16384` の整数平方根を最も近い整数へ丸めたものである。`floor_isqrt` は整数だけである。f32 の `sqrt` は、初期値にすら使わない。Rust の `f32::sqrt`、JS の `Math.sqrt`、WGSL の `sqrt` は禁止する。`n = d * 16384` は 2^28 以下で u32 に入る。`floor_isqrt` は u32 の二分探索である。`lo = 0`、`hi = 16384` から、`mid = (lo + hi + 1) / 2` とし、`mid * mid <= n` なら `lo = mid`、そうでなければ `hi = mid - 1` とする。`16384² = 2^28` なので積は u32 に入る。隣り合う平方の間隔 `2r + 1` は奇数なので、整数の平方根に結びはない。`r = floor_isqrt(n)`、`rem = n - r²` として、`rem >= r + 1` のとき `r + 1` を採る。結果は 0..=16384 に切る。三実装がこの整数手順だけを使う。

`src <= 8192` のとき `B = d - div_q((16384 - 2 src) * d * (16384 - d), 16384²)`。そうでないとき `B = d + div_q((2 src - 16384) * (d2 - d), 16384)`。`d2 - d` は丸めで負になり得る。差は i64 で持ち、`B` を最後に 0..=16384 へ切る。

**difference（10）。** `B = abs(dst - src)`。除算はない。

**exclusion（11）。** `B = clamp(dst + src - div_q(2 * dst * src, 16384), 0, 16384)`。`2 * 16384²` は i32 に入る。計算は i64 で揃える。

**hue（12）/ saturation（13）/ color（14）/ luminosity（15）。** 輝度は `Q14_LUMA`（3483, 11718, 1183）で `div_q(r * 3483 + g * 11718 + b * 1183, 16384)` である。ストレートが 0..=16384 のとき、この積の和は 16384² で i32 に入る。彩度は `max - min`。

`set_sat(c, s)` は `max == min` なら 0。そうでなければチャネルごとに `div_q((c - min) * s, max - min)`。結果は 0..=s である。

`set_lum(c, l)` は `clip(c + (l - lum(c)))` である。入力の `c` と `l` が 0..=16384 のとき、クリップ前のチャネルは −16384..=32768 である。輝度もその範囲にいる。

`clip` は次のとおり。`n` は最小、`x` は最大、`l` はその色の輝度である。

- `n < 0` のとき、`l == 0` なら結果は 0。そうでなければ `l + div_q((c - l) * l, l - n)`。
- `x > 16384` のとき、`x == l` ならそのまま。そうでなければ `l + div_q((o - l) * (16384 - l), x - l)`。
- 各チャネルを 0..=16384 に切る。

`(c - l) * l` の絶対値は、この範囲で `6 * 16384² = 1_610_612_736` までで、i32 に入る。これも i64 の `checked_mul` で計算する。

モードの対応は float 式と同じである。

- hue: `set_lum(set_sat(src, sat(dst)), lum(dst))`
- saturation: `set_lum(set_sat(dst, sat(src)), lum(dst))`
- color: `set_lum(src, lum(dst))`
- luminosity: `set_lum(dst, lum(src))`

SwiftShader の v2 `blend-color` が 1 画素ずれるのは、この color の経路である。v3 では f32 のストレート化と f32 の輝度除算を通さない。

normal 以外は、宛先をストレート化して `B` を求め、上の三項の和でプレマルチプライへ戻す。

## 溢れの検査

テスト名は `composite_q14_blend_does_not_overflow` である。`color_adjust_q14_mix_does_not_overflow` と同じで、`checked_mul` / `checked_add` が `None` を返したら失敗する。JS は BigInt なので溢れない。WGSL は同じ範囲が `Wide` の 64 ビットに入ることを、Rust 側のこのテストが境界として押さえる。

刻み 512 の標本は証明にしない。上限は上の解析である。三項の分子は `16384³ = 4_398_046_511_104` 以下、soft-light の `16 d³` は `d <= 4096` で `16 * 4096³ = 1_099_511_627_776` 以下、`clip` の `(c - l) * l` はチャネルが −16384..=32768 のとき絶対値 `6 * 16384² = 1_610_612_736` 以下である。いずれも i64 に入り、三項の分子と soft-light の立方は i32 に入らない。

テストは端点の直積を尽くし、checked の `None`、解析上限の超過、`out_rgb > out_a` を失敗にする。端点は `{0, 1, 4095, 4096, 4097, 8191, 8192, 8193, 16383, 16384}` である。4096 と 8192 は soft-light と overlay の分岐、その隣は分岐の両側である。

- チャネル独立のモードは、ストレート src、ストレート dst、src アルファ、dst アルファ、そして `B` をこの端点から取る直積である。`dp <= da` の点だけで `out_rgb <= out_a` を見る。分子は端点のすべてで `16384³` 以下である。
- hue / saturation / color / luminosity はチャネルが分かたれない。各色の 3 チャネルをこの端点の直積にし、二つの色と二つのアルファも端点の直積にする。
- soft-light の多項式は `d` の 0..=4096 を全数、checked で計算する。標本にしない。`16 * d * d * d` が `1_099_511_627_776` を超えたら失敗する。
- `clip` はチャネルの端点 `{−16384, −1, 0, 1, 8192, 16384, 16385, 32768}` の直積で checked し、`(c - l) * l` の絶対値が `1_610_612_736` を超えたら失敗する。結果は 0..=16384 に切る。

color-adjust の彩度 mix の範囲 −5_502_926_848..=5_771_362_304 は `color_adjust_q14_mix_does_not_overflow` が持つ。ここには再掲しない。

## 検証

一致は全バイト一致である。バックエンドは次の三つで、v2 の計測と同じジョブである。

- Metal。native ジョブ（macOS）の `cargo test`。CI のアダプタは `Apple Paravirtual device` である。実機の Apple GPU では未検証であり、ここでの一致は実機の挙動を主張しない。
- lavapipe。gpu-parity ジョブ。
- SwiftShader。wasm ジョブ。`ALTO_BROWSER_GPU=swiftshader` で、アダプタの architecture に `swiftshader` が含まれること。

アトラス読み取りも同じ三つで検査する（必須）。256 通りのバイト値を全チャネルに入れた `Rgba8Unorm` テクスチャを `textureLoad` + `unorm_byte` で読み、全値が正確に戻ることをバックエンドごとに記録し、確かめる（Rust `Gpu::atlas_read`、出力 `atlas-read.json` / `atlas-read.md`。ブラウザは `tests/e2e/parity-tests.js` の `measureAtlasRead`、`parity.json` の `atlasRead`）。これは enforced（必須）である。どれかのバックエンドで一つでも戻らない値やエラーがあれば、`gpu_shader_matches_cpu_reference`（Metal、lavapipe）とブラウザのパリティ実行（SwiftShader）が失敗する。結果の JSON は失敗時も残す。不一致が出たら、そのときに `Rgba8Uint` を検討する。

対象は、半透明の `-alpha-measure` ケース全部である。v3 の文書として実行し、一致したら v3 のハッシュとして固定する。v2 の `-alpha-measure` は measured-not-enforced のままである。`tests/parity/render-v1.txt` と `tests/parity/cpu-reference.txt` の既存行は一文字も変えない。`existing_v1_and_v2_hashes_are_unchanged` がそれを見る。

特に、SwiftShader の v2 `blend-color-alpha-measure` が 1 画素だけ違った点を、v3 では一致させる。画素 (181, 82)、ソース `181,82,185,254 | 99,202,198,128`、v2 の SwiftShader は GPU `105,107,157,255`、CPU `105,107,156,255` だった。Metal の `opacity-half-alpha-measure` と `nested-groups-alpha-measure`（各 4098 画素、アルファが 0.5/255 の結びで GPU が 1、CPU が 0）も、v3 では一致させる。あれは unorm ストアの差なので、整数の最終バイトでは起きない。

v3 を既定にする変更は、この一致のあとである。この設計の実装ステップには含めない。一致するまでは、バージョン 3 へ上げる操作と format 4 の保存は main の不安定オプトインが要る。v2 の式は凍結したまま、既存文書の対処は `render.upgrade` である。
