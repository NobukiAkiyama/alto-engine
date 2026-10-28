# v3 整数マスク

作成日：2026-10-28（Asia/Tokyo）

状態：設計のみ。この文書は実装しない。マスクの v3 は、[v3 整数合成](2026-10-28-v3-integer-compositing.md)（PR [#24](https://github.com/NobukiAkiyama/alto-engine/pull/24)、main の `5ce300e`）と同じ不安定オプトインの向こうに置く。描画バージョン 1 と 2 のマスクの式は凍結したままである。

関連：[v3 整数合成](2026-10-28-v3-integer-compositing.md)、[正確な CPU 参照](2026-10-27-exact-cpu-references.md)、[画像編集](IMAGE-EDITING.md)、[半透明合成の計測](../verification/semi-transparent-composite-parity.md)。color-adjust の Q14 は PR [#22](https://github.com/NobukiAkiyama/alto-engine/pull/22)（main の `adf70b3`）である。

## 前提

マスクは合成パス（`composite.wgsl`）を通らない。`plain_scene`（`crates/alto-scene/src/damage.rs`）は `features.mask` があるシーンを外し、M5 の効果パス（`crates/alto-gpu/src/renderer/m5.rs`、シェーダは `effects.wgsl`、中間は `rgba16float`）が描く。マスクを掛ける文は次の五つである。

| モード | 呼ぶ場所（`m5.rs`） | v2 の式（`effects.wgsl`） |
| --- | --- | --- |
| 4 | `apply_props`。レイヤーマスクと不透明度 | `load_a(p) * mask * c.settings.x`。`mask` は `sparse(p).r`、マスクがなければ 1 |
| 5 | `stage` と `stack`。効果の `selection` | `mix(load_a(p), load_b(p), sparse(p).r)` |
| 7 | `children`。調整レイヤーのマスク・不透明度・ブレンド | `amount = opacity * sparse(p).r`。非 normal はストレートの積（`settings.w == 2` だけ screen）。最後に `mix(original, filtered, amount)` |
| 11 | `vector_coverage` と `apply_props`。ベクターマスクとクリッピングパス | `load_a(p) * load_b(p).r` |
| 12 | `children`。調整レイヤーへのベクターのカバレッジ | `mix(load_a(p), load_b(p), load_clip(...).r)` |

マスクの中身は R8 である。`validate_mask`（`crates/alto-doc/src/lib.rs`）が R8 以外を拒否する。アトラスへは `rgba_pixels`（`crates/alto-gpu/src/renderer.rs`）とアップロードが `[v, v, v, 255]` に広げ、`Rgba8Unorm` のアトラスに置く。`sparse` は f32 の逆行列 `c.inv0` / `c.inv1` と `floor` で最近傍のテクセルを引き、`.r` を読む。

feather、反転、選択の add / subtract / intersect は、描画ではなく編集である。`selection_ops` と `generated_selection`（`crates/alto-engine/src/m5/mod.rs`）が f64 で計算し、`(coverage * 255.0).round() as u8` で R8 に焼く。`mask_for_node` は選択を R8 のまま最近傍でレイヤー座標へ写す。マスクのブラシは、`mask: true` のストロークを読み戻した R8 である（`tiles_from_mapped`、`crates/alto-gpu/src/renderer/readback.rs`）。どれも結果は内容ハッシュを持つ R8 タイルとして文書に入る。ピアが受け取るのは式ではなくバイトである。したがって描画の決定性は R8 のバイトから始まり、v3 はマスクの生成を変えない。

輝度やアルファからマスクを作る経路は main にない。`set_mask`（`crates/alto-wasm/src/lib.rs`）の source は `"white"`、`"selection"`、`"remove"` だけで、RGBA のマスクは拒否される（`invalid_effect_and_rgba_mask_are_atomic`）。反転は選択の `"invert"` だけで、生成時に焼く。描画時の反転、輝度マスク、アルファマスクの式は、この文書では定義しない。

parity の `mask` が xfail なのは、このケースが v2 の文書（`createDocument`）で、M5 の f16 中間を通り、CPU 参照にマスクがないからである。Rust の `gap`（`crates/alto-gpu/src/cpu_composite.rs`）と JS の `sceneGap`（`tests/e2e/cpu-composite.js`）は `"mask"` を返す。

[正確な CPU 参照](2026-10-27-exact-cpu-references.md) の「mask」節は、v2 のまま f16 と Q8 で書く案だった。この文書がそれを置き換える。理由は二つある。#24 は unorm ストアを決定性の基礎にしないと裁定した。Q8 は、v3 の Q14 の隣に二つ目の数の体系を作る。v2 のモード 4 の文は、あの節のとおり変えない。

## 範囲

| 対象 | 対象外 |
| --- | --- |
| レイヤーマスク（`RFeatures.mask`、モード 4） | マスクの生成（選択、feather、反転、ブラシ）。上のとおり編集である |
| 効果の選択（`REffect.selection`、モード 5） | クリッピング（`features.clip`、モード 9 と 10） |
| 調整レイヤーのマスク（モード 7） | ベクターマスクとクリッピングパスのカバレッジの生成。vector-fill と gaussian-blur の PR の範囲 |
| モード 11 と 12 の乗算の式。カバレッジの Q14 を受け取ったあとだけ | マスク塗り中のプレビュー（`mask_preview`）。parity の対象にしない |

## main と #24 から使うもの

新しい丸めは作らない。結びは 0 から遠い側へ行く。マスクの式の項はすべて 0 以上なので、実際に通るのは「除数の半分を足して切り捨てる」側だけである。

| 名前 | 場所 | ここでの役割 |
| --- | --- | --- |
| `unorm_byte` | `crates/alto-gpu/shaders/effects.wgsl`。Rust の計測側は `cpu_composite.rs` の `UNORM_BYTE_WGSL` | アトラスの `.r` をバイトに戻す |
| `byte_q14` / `byteQ14` | WGSL `effects.wgsl`、Rust `cpu_composite.rs`、JS `cpu-composite.js` | マスクのバイトを Q14 のカバレッジにする |
| `premul_byte_q14` / `premulByteQ14` | 同上 | レイヤーの入口（モード 18 の置き換え先）。マスクの式では使わない |
| `div_q` / `div_q_i64` / `divQBig` | `effects.wgsl` / `cpu_composite.rs` / `cpu-composite.js` | 16384 での除算。Q28 での除算も `div_q_i64` と `divQBig` が除数 `1 << 28` で受ける |
| `Wide`、`wide_mul_u32`、`wide_add` | `effects.wgsl` | u32 に入らない積。マスクの積は負にならないので `wide_mul_i32` と `wide_neg` は要らない |
| `integer_inverse`、`pack_wide`、`SAMPLE_INTEGER` | `crates/alto-gpu/src/sample.rs` | マスクを引く位置の Q16 の逆行列。v3 のマスクは常にこれを使う |
| `pixel_q8`、`source_q8`、`floor_frac_q8` | `sample.rs`（テスト用） | Rust の参照の位置 |
| `pixel_q8`、`coord_q8`、`wide_round_shr16` | `crates/alto-gpu/shaders/composite.wgsl` | WGSL の位置。`effects.wgsl` は別シェーダなので、`Wide` と同じく写す |
| `quantizeInverse`、`pixelQ8`、`sourceAxis`、`floorFracQ8` | `cpu-composite.js` | JS の参照の位置 |
| `Gpu::atlas_read`、`measureAtlasRead` | `cpu_composite.rs`、`tests/e2e/parity-tests.js` | アトラス読み取りの必須検査。マスクもこのアトラスを読む |
| `measureRow`、`formatMeasure` | `parity-tests.js` | measured-not-enforced の行 |
| `color_adjust_gradient` / `colorAdjustGradient` | `cpu_composite.rs` / `cpu-composite.js` | 全 RGB と、アルファ 0、1、128、254 を持つ入力画像 |
| `RENDER_VERSION_EXACT` | `crates/alto-core/src/lib.rs` | `m5.rs` の `blend`、`stage`、`raw_node` と同じ分岐 |

main にまだないものは三つある。ホストの不透明度の Q14 量子化（文書の f32 を 0..=1 に切り、`color_adjust_q14` と同じ「0.5 を足して 0 へ向かって切り捨てる」規則で 0..=16384 にする）と、WGSL の 16384² 除算（絶対値に 2^27 を足して 28 ビットシフトする）は、#24 の v3 composite の実装 PR で入る。三つ目の M5 の Q14 中間（`rgba16uint`）と整数の出力は、その次の PR で入る（下の「実装順」の (c)）。マスクはこれを共有し、別の版を書かない。

## 値の置き場と順番

マスクの v3 は、M5 の v3 の中間が `rgba16uint` に切り替わったあとで実装する（下の「実装順」の (c)）。中間はプレマルチプライ Q14（0..=16384）で、`rgba16uint` のテクスチャに置く。0..=16384 は u16 に収まり、整数なので変換もフィルタもない。読むのは `textureLoad` だけで、`textureLoad` を三本読む今の M5 の形を保つ。タイル出力は `div_q_i64(q * 255, 16384)` のバイトを `rgba8uint` に書く。マスクのモードはその Q14 を読み、同じ形で書く。`rgba16float` へは書かない。

パック u32 のストレージバッファは採らない。M5 の大きな書き換えになるうえ、`rgba16uint` に比べて決定性の利得がない（レビュー、Linus）。

#24 の文言はこれに合わせて次のように改訂する。「最終のタイル出力は `rgba8uint` かパック u32。中間値は Q14 の整数で `rgba16uint` を許す。float の中間は v3 のどこにも置かない」。改訂自体はこの文書ではなく、v3 composite の実装 PR で入る。

color-adjust のように f16 の中間（`q14_stored`）と `effects-resolve.wgsl` の unorm 解決の上で先に入れることはしない。その経路は Q14 ではなく、しかも結びが一つだけあり、マスクはそこを踏む。

- `q14_to_f16_bits` の出力は、いつも Q14 の整数である。q < 2048 は f16 に正確に入る。q >= 2048 は仮数 11 ビットへの丸めで 2、4、8 の倍数になる。どちらも整数である。
- しかしこれは Q14 の精度落ちである。q >= 2048 で下位の 1〜3 ビットが消える。したがって main の color-adjust の v3（#22）は真の Q14 ではなく、Q14 と名乗る f16 精度の値である。#22 の 20 ケースが一致したのは、出力が下の結びを踏まず、その精度落ちを CPU 参照が同じように再現したからにすぎない。Q14 の算術が一致したことは示していない。
- `effects-resolve.wgsl` は f16 を `rgba8unorm` へ `textureStore` する。値 q/16384 が結びになるのは `q * 255 / 16384 = n + 1/2`、すなわち `255 q = (2n + 1) * 8192` のときである。255 は奇数なので q は 8192 の奇数倍で、0..=16384 の中では 8192 だけである。
- 不透明な画素に不透明度 0.5 のマスクを掛けると、`F = 16384 * 8192 = 2^27`、出力は 8192 になる。ちょうどその結びである。Metal は `rgba8unorm` の結びを CPU と違う側へ送ると計測されている（[半透明合成の計測](../verification/semi-transparent-composite-parity.md)、8208 チャネル）。

したがって f16 の段階を挟むと、不透明度 0.5 を enforce できない。整数の出力では、`div_q_i64(8192 * 255, 16384)` は 0 から遠い側の 128 で一意である。

## v3 の範囲：モード 22 と 23

この PR は、マスクのほかに v3 の範囲を二つ広げる。どちらも `effects-q14.wgsl` の整数のモードで描き、`v3_gap`（Rust）と `sceneGap`（JS）が受け入れる。

- **モード 22。選択つきの color-adjust。** v2 のモード 5 にあたる。効果の `selection` を持つ color-adjust を、上の「モード 5」の式で描く。color-adjust 以外の効果の選択は、今までどおり拒否する。
- **モード 23。normal 以外のブレンドの調整レイヤー。** v2 のモード 7 にあたる。マスクがなくても、ブレンドが normal でない color-adjust の調整レイヤーはこのモードで描く。式は上の「モード 7」である。ただし、マスクなしは v3 では拒否する（float purge PR まで。上流のモード 18・17・19 と resolve に f32 が残るため、`v3_gap` / `sceneGap` は v3-unsupported、ホストは GpuUnsupported とし、float では描かない）。

**v3 では、screen 以外のブレンドはすべて multiply で描く。** モード 23 の分岐は screen（2）と multiply の二つだけで、overlay、darken、lighten などのほかのモードも multiply の式を通る。これは v2 のモード 7 の意味をそのまま保ったもので、仕様として凍結する。v3 の調整レイヤーで、ブレンドモードごとの式を後から入れることはしない。入れるなら、新しい描画バージョンの変更として扱う。

## マスクのバイトを読む

アトラスは #24 の裁定どおり `Rgba8Unorm` 一枚のままにする。マスク用の `Rgba8Uint` は作らない。

- `textureLoad` で読み、`.r` だけを `unorm_byte` でバイトに戻す。G、B、A は読まない。`textureSample`、サンプラ、ハードウェアのフィルタは使わない。
- 最近傍だけである。v2 の `sparse` も最近傍なので、意味は変わらない。v3 でマスクに 4 タップの補間を入れない。
- タイルの探し方は `sparse` と同じ二分探索である。マップにないタイルはバイト 0、スロットが `0xffffffff` のタイルは v2 と同じ `(1, 0, 0.2, 1)` で、`.r` はバイト 255 になる。
- その後はすべて整数である。`unorm_byte` の往復は `Gpu::atlas_read` と `measureAtlasRead` が 256 値の全チャネルで必須の検査として押さえる。`.r` はそのチャネルの一つなので、新しいアトラスの検査は足さない。

引く位置は常に整数である。ホストの `mask_sampler`（`crates/alto-gpu/src/renderer.rs`）がマスクの逆変換（`mask_entries` の `transform` の逆）を `integer_inverse` で Q16 にし、`SAMPLE_INTEGER` で引く。軸平行で倍率 1 の逆変換も同じ経路を通る。v3 のマスクに f32 の位置（`sparse`）はない。

- 画素中心は `pixel_q8`、ソース位置は `coord_q8`（Rust `source_q8`、JS `sourceAxis`）。テクセルは Q8 の算術右シフト `>> 8`（`floor_frac_q8` の床）で、負の座標でも床になる。タイルキーはテクセルの `>> 8`、タイル内の位置はテクセルからキー × 256 を引いたものである。重み `fx, fy` は使わない。
- `pixel_q8` は整数の原点と倍率を要る。`Roi`（`m5.rs`）の `x`、`y`、`scale` は f32 である。三つがちょうど整数で倍率が 1 以上であることを要る。
- 整数で表せないマスク（逆変換がない、`integer_inverse` が `None`、整数でない原点、倍率が 1 未満）は、描画を `GpuUnsupported` で拒否する（DQ3-4）。float で描くことはしない。CPU 参照も同じ条件で拒否する（Rust `mask_byte_cpu` は `None`、JS `maskByteV3` は例外）。

## 式

記号は次のとおり。`v` はプレマルチプライ Q14 の一チャネル（RGB もアルファも同じ式）、`b` はマスクのバイト、`m = byte_q14(b)`、`o` はホストが量子化した不透明度（0..=16384）。マスクがない、または `mask_enabled` が偽のとき（このとき `features.mask` は `None` である）、`m = 16384` とする。

係数は Q28 の一つの整数にまとめる。

```text
F = m * o                       // 0..=2^28
div28(n) = (n + 2^27) >> 28     // n >= 0。#24 の 16384² 除算
```

`m` と `o` を別々に 16384 で割らない。丸めは一回だけである。v2 の `load_a(p) * mask * c.settings.x` は f32 の積の順に意味があったが、整数の積は可換で正確なので順は残らない。

**モード 4（レイヤーマスクと不透明度）。** 四つのチャネルとも

```text
out = div28(v * F)
```

性質は三つある。`F = 0` なら 0。`F = 2^28` なら `v` そのもの。`m = 16384` なら `div28(v * 16384 * o) = div_q(v * o, 16384)` である。最後の等式は、`(2^14 X + 2^27) / 2^28` と `(X + 2^13) / 2^14` が同じ有理数なので床も等しい、ということである。つまりマスクのないモード 4 は、#24 の「不透明度が 1 でないときは、その Q14 を一度だけ掛ける」と同じ整数になる。

**モード 5（効果の選択）。** `s = byte_q14(b)`、`in` は効果の前、`f` は効果の後。

```text
out = div_q(in * (16384 - s) + f * s, 16384)
```

`s = 0` なら `in`、`s = 16384` なら `f` が正確に戻る。分子は 16384 の倍数なので丸めが起きない。

**モード 7（調整レイヤー）。** `F = m * o`。v2 と同じく、ホストはクリップのアルファをここで掛けない（`children` が `p.settings[2] = 0.0` にする）。`orig` は調整前、`filt` は調整後である。

- ブレンドが normal なら `g = filt`。
- そうでないときは、#24 のストレート化（アルファ 0 なら 0、そうでなければ `clamp(div_q(p * 16384, a), 0, 16384)`）で `d = straight(orig)`、`t = straight(filt)` を作る。ブレンドが screen（2）なら `c = 16384 - div_q((16384 - d) * (16384 - t), 16384)`、それ以外は `c = div_q(d * t, 16384)` である。`g_rgb = div_q(c * orig_a, 16384)`、`g_a = orig_a`。
- v2 は screen 以外の非 normal を全部 multiply にしている（`u32(c.settings.w) == 2u` の分岐だけがある）。v3 はこの意味を変えない。変えるのは算術だけである。これは仕様として凍結する（「v3 の範囲：モード 22 と 23」）。

```text
out = div28(orig * (2^28 - F) + g * F)
```

**モード 11 と 12（ベクターのカバレッジ）。** カバレッジの Q14 を `k` とする。モード 11 は `out = div_q(a * k, 16384)`、モード 12 はモード 5 の `s` を `k` にした式である。`k` を作るのは vector-fill と gaussian-blur（feather は `path_coverage` が掛けるガウスぼかし）の v3 で、この文書ではない。それまで v3 の文書でも、ベクターマスクとクリッピングパスは v2 のモード 11 と 12 のままにする。#24 が color-adjust の非 normal を v2 の float に戻しているのと同じ扱いで、その v3 のハッシュは最終ではない。

**`min(rgb, a)` は入れない。** どのモードも、入力が `rgb <= a` なら出力も `rgb <= a` である。

- モード 4。`F` を固定すると `n -> div28(n)` は単調非減少である。`v_rgb <= v_a` から `v_rgb * F <= v_a * F` で、丸めたあとも順が残る。
- モード 5 と 12。重みが 0 以上で、`in_rgb <= in_a` と `f_rgb <= f_a` から分子の順が残る。
- モード 7。`c <= 16384` なので `g_rgb = div_q(c * orig_a, 16384) <= orig_a = g_a`。残りはモード 5 と同じである。

## グループと孤立

マスクを持つグループは、M5 の経路で必ず自分のターゲットへ描かれる。`children` の素通し条件（`pass`）が `node.features.mask.is_none()` を要るからである。#24 の孤立の条件 `isolated || opacity < 1.0 || mode != normal` が偽でも、マスクがあれば孤立する。この規則は変えない。

順番は次のとおりである。

1. 子を透明な Q14 のターゲットへ #24 のブレンドで合成する。
2. `apply_props` のモード 4 で `F = m * o` を一度だけ掛ける。グループの不透明度はここで消費する。
3. `blend`（`m5.rs`）が `p.settings[0] = 1.0` でグループのブレンドモードを使い、親へ合成する。不透明度を二度掛けない。

#24 の合成パスのポップ（ストレート化、アルファだけに不透明度、ブレンド）と、M5 のこの順（プレマルチプライに `F`、ブレンドでストレート化）は、半透明のグループでは丸めが違い得る。経路が違うので、ビット一致は主張しない。主張するのは次の一つだけである。

- 恒等変換の normal のラスターで、マスクが全画素 255 かマスクなし、不透明度が任意なら、M5 の結果は #24 の合成パスと全バイト一致する。#24 のラスターの入口は `div_q(premul_byte_q14(color, alpha) * o, 16384)` で、normal はそのプレマルチプライをストレート化せずに載せる。M5 はモード 18 の置き換え先が `premul_byte_q14`、モード 4 が上の性質で `div_q(· * o, 16384)`、ブレンドが同じ normal の式である。

入れ子のマスクは、各段のモード 4 が Q14 の 0..=16384 に戻すので、段を重ねても値の幅は増えない。深さは `MAX_TREE_DEPTH` が抑える。マスクのあるクリッピングの土台は、`apply_props` が先にマスクを掛け、そのあとモード 9 と 10 が働く。モード 9 と 10 は対象外なので、マスクとクリッピングを組み合わせたケースは enforce しない。

## 溢れの上限

すべての項は 0 以上である。上限は解析で、標本ではない。

| 項 | 上限 | 入る型 |
| --- | --- | --- |
| `byte_q14` の `b * 16384 + 127` | `255 * 16384 + 127 = 4_177_967` | u32 |
| `F = m * o` | `16384² = 268_435_456` | u32、i32 |
| モード 4 の `v * F + 2^27` | `2^42 + 2^27 = 4_398_180_728_832` | i64、`Wide`。u32 には入らない |
| モード 5 と 12 の分子 + 8192 | `16384 * (16384 - s) + 16384 * s + 8192 = 268_443_648` | i32 |
| モード 7 の `orig * (2^28 - F) + g * F + 2^27` | `16384 * 2^28 + 2^27 = 4_398_180_728_832` | i64、`Wide` |
| モード 7 のストレート化 `p * 16384`、積 `d * t`、`c * orig_a` | `2^28` | i32 |
| モード 11 の `a * k` | `2^28` | i32 |
| 出力の `q * 255` | `4_177_920` | i32 |

モード 4 の上限は `v = 16384, F = 2^28` で等号になり、#24 の `16384³` と同じ値である。モード 7 は `(2^28 - F) + F = 2^28` なので、`orig` と `g` を独立に最大にしても `16384 * 2^28` を超えない。

WGSL は `wide_mul_u32(v, F)` で積を作り、`wide_add` で `Wide(1u << 27u, 0u)` を足し、`(lo >> 28u) | (hi << 4u)` を商にする。和は 2^43 未満なので `hi` は 2^11 未満で、商は 16384 以下である。モード 7 は `wide_mul_u32` を二つ `wide_add` で足してから同じ段を通す。項が負にならないので、`wide_div_q14` のような絶対値と符号の分岐は要らない。

## 溢れの検査

テスト名は `mask_q14_does_not_overflow` で、`cpu_composite.rs` に置く（新規）。`color_adjust_q14_mix_does_not_overflow` と同じく、`checked_mul` / `checked_add` が `None` を返したら失敗する。そのほか、分子が上の表の上限を超えたとき、出力が 16384 を超えたとき、`rgb <= a` の入力で `out_rgb > out_a` になったときに失敗する。JS は `divQBig` の BigInt である。WGSL が同じ範囲を `Wide` に入れることは、このテストが境界として押さえる。

端点は #24 と同じ `E = {0, 1, 4095, 4096, 4097, 8191, 8192, 8193, 16383, 16384}` である。マスクのバイトは 256 通りしかないので、モード 4 と 5 では全部を尽くす。

- `byte_q14` は 256 バイトすべてで狭義単調増加、`0 -> 0`、`255 -> 16384`。
- モード 4。`v_rgb <= v_a` の `E` の組（55 組）× `m = byte_q14(0..=255)` × `o ∈ E`。
- モード 4 の恒等。`v` を 0..=16384 の全数、`m = 16384`、`o ∈ E` で、`div_q_i64(v * o, 16384)` と等しいこと。`F = 0` と `F = 2^28` も全数の `v` で見る。
- モード 5。`in` と `f` の順序付きの組（各 55 組）× `s = byte_q14(0..=255)`。`s = 0` と `s = 16384` で入力がそのまま戻ること。
- モード 7。`orig` と `filt` の順序付きの組（各 55 組）× `m = byte_q14({0, 1, 127, 128, 254, 255})` × `o ∈ E` × ブレンド `{0, 1, 2, 3}`。3（overlay）は v2 の multiply への落ち方を押さえる。
- 位置。負の Q8 座標 `{-257, -256, -255, -1, 0, 255, 256}` で、`>> 8` が `div_euclid(256)` と一致すること。

## 一致の期待

一致は全バイト一致である。バックエンドは #24 と同じ三つである。Metal は native ジョブ（`Apple Paravirtual device`、実機は未検証）、lavapipe は gpu-parity ジョブ、SwiftShader は wasm ジョブ（アダプタの architecture に `swiftshader`）。ブラウザは color-adjust と同じく `tests/fixtures/render-v3-empty.alto` を開いて v3 の文書にする。

一致を期待する理由は一つである。`unorm_byte` のあとはすべて整数の加減乗と、非負の整数除算である。引く位置も Q16 と Q8 の整数である。WGSL の整数演算は仕様で一意で、上の表で溢れがないことを示した。

| ケース | 中身 | WGSL = Rust = JS | Metal | lavapipe | SwiftShader | 扱い |
| --- | --- | --- | --- | --- | --- | --- |
| `mask` を v3 で | 既存 xfail と同じ画像（不透明の赤、`x < 128` の矩形選択、不透明度 1） | 一致 | 一致 | 一致 | 一致 | enforce |
| カバレッジ全値 | 入力は `color_adjust_gradient`。マスクは 0..=255 の全バイト。不透明度 `{1, 0.5, 0.35, 0}` | 一致 | 一致 | 一致 | 一致 | enforce。0.5 は上の結びのケース |
| 全白マスクと合成パス | 恒等の normal ラスター、全画素 255 のマスク、不透明度 `{1, 0.5, 0.35}` を、マスクなしの v3 合成と比べる | 一致 | 一致 | 一致 | 一致 | enforce。#24 の合成パスが入ってから |
| 選択つき color-adjust | モード 5。入力は勾配、選択は矩形の feather | 一致 | 一致 | 一致 | 一致 | enforce |
| マスクつき調整レイヤー | モード 7。color-adjust、ブレンド `{normal, multiply, screen, overlay}`、不透明な背面 | 一致 | 一致 | 一致 | 一致 | enforce。半透明の背面は #24 の M5 のブレンドが入ってから |
| 回転したマスク | リンクしないマスクを 0.3 rad 回し、恒等のレイヤーへ。`SAMPLE_INTEGER` | 一致 | 一致 | 一致 | 一致 | enforce |
| 位置の結び | 平行移動 (2.501953125, -0.498046875) の倍率 1 のマスク。Q8 の和がちょうど `.5` になり、負の側はタイルキー -1 へ越える | 一致 | 一致 | 一致 | 一致 | enforce |
| マスクつきグループ | 半透明の子を持つグループにマスク | 一致 | 一致 | 一致 | 一致 | enforce。#24 の M5 のブレンドが入ってから |
| ベクターマスク、クリッピングパス | MSAA とガウスのカバレッジ | 期待しない | — | — | — | measured-not-enforced |
| 選択つきのぼかし、levels、curves、hue-saturation、invert | 効果側が v2 の float | 期待しない | — | — | — | measured-not-enforced |
| 整数で引けないマスク | `integer_inverse` が `None`、逆変換がない、整数でない原点、負のレベル | — | — | — | — | 描画を拒否する（DQ3-4） |
| v2 の `mask` | `createDocument` のまま | — | — | — | — | xfail のまま |

Rust の参照は、R8 タイルをシーンに直接置ける（`plain_scene_gaps_are_explicit` と同じ組み方）。カバレッジ全値のケースは、Rust では x がバイトになる R8 タイルを置く。ブラウザは feather つきの矩形選択で 0..=255 を作る。JS の参照は、そのバイトを `selection_ops` の矩形と同じ f64 の式で作り直す。使う演算は加減除、`min`、`clamp`、非負の値の `round` だけで、どれも IEEE で一意である。`Math.round` と Rust の `round` が違うのは負の結びだけで、ここには現れない。楕円と投げ縄は `hypot` を使うので parity に入れない。

レベルは 0 だけを比べる。いまの parity と同じである。

## enforce と measure

enforce するのは、構成上一致し、3 バックエンドの CI で全バイト一致したケースだけである。上の表で enforce と書いたものは、実装 PR で一致を確かめてから通常のケースとして入れる。期待どおりに一致しなかったら、measure に落とさない。設計に戻る（#24 のアトラス読み取りと同じ扱い）。前提の #24 の実装と「実装順」の (c) が入っていないケースは、入るまで足さない。

measure するのは、算術が整数でも入力が float のまま来るケースである。`measureRow` と `formatMeasure` で `measure: true, enforced: false` の行にし、v2 の `-alpha-measure` と同じく失敗にしない。アトラス読み取り（`Gpu::atlas_read`、`measureAtlasRead`）は今のまま必須である。

## v1 / v2 を動かさない

- `effects.wgsl` のモード 4、5、7、11、12 の文は一文字も変えない。v3 は新しいモード番号を使う。0..=19 は再利用しない。6 も `m5.rs` の `Self::params(6, …)` が使っているので避ける。
- ホストの分岐は `self.scene.render_version >= alto_core::RENDER_VERSION_EXACT` である。`blend`、`stage`、`raw_node` と同じ形にする。
- マスクの生成（`selection_ops`、`generated_selection`、`mask_for_node`、ブラシ）は変えない。文書の R8 バイトと内容ハッシュは、どのバージョンでも動かない。
- `tests/parity/render-v1.txt` と `tests/parity/cpu-reference.txt` の既存行は一文字も変えない。v3 の行を足すだけである。`existing_v1_and_v2_hashes_are_unchanged` がそれを見る。v2 のマスクにはロック済みのハッシュがない（xfail）が、出荷済みの v2 の文書の画素は、文を変えないことで守る。
- `plain_scene` と `gap` は変えない。マスクのあるシーンは v3 でも合成パスに乗らない。`plain_scene_gaps_are_explicit` はそのまま通る。
- ゲートは #24 のとおりである。Rust はフィーチャ `unstable-render-v3` かこのクレートのテスト、JS は `{ unstableRenderV3: true }` で、`UNSTABLE_RENDER_V3` は false のままである。
- シェーダが変わるのでリリース WASM のサイズとハッシュの記録は動く。#22 と同じく記録し直す。

## 実装順

前提の PR はレビュー（Linus）で次の順に合意した。(a) から (c) はこの文書の範囲ではない。

- (a) measure-only の PR。#22 の color-adjust v3 の 20 ケースで、Q14 の 8192 を生む画素の数をバックエンドごとに数える。enforce は変えない。
- (b) v3 composite の実装 PR（#24 の設計）を続ける。#24 の文言の改訂（上の「値の置き場と順番」）もここで入る。
- (c) 次の PR で、M5 の v3 の中間を `rgba16uint` に切り替える。v3 の経路から `q14_to_f16_bits`、`f16_bits_to_q14`、`effects-resolve.wgsl` の unorm 解決を除き、color-adjust の v3 を enforce し直し、不透明度 0.5（8192）の parity ケースを足す。v3 のハッシュは変わる（まだ最終ではない）。v1 と v2 の経路は変えない。
- (d) マスクの実装。(c) に依存し、その後に入る。以下はその中身である。

1. 前提。(c) の M5 の Q14 中間（`rgba16uint`）と整数の出力。
2. Rust の参照と `mask_q14_does_not_overflow`。
3. JS の参照（`divQBig`、`byteQ14`、`pixelQ8`、`sourceAxis`、`floorFracQ8`）。
4. WGSL の新しいモードと、`composite.wgsl` から写す `pixel_q8`、`coord_q8`、`wide_round_shr16`。ホストの分岐。
5. 上の enforce のケースを 3 バックエンドで流し、一致を記録してから固定する。
6. measure の行。

## 未解決の問題

番号 1 と 2 はレビュー（Linus）で決定済みである。残る 3 から 10 は開いたままで、実装 PR で決める。

1. **M5 の中間の形。決定済み。** M5 の v3 の中間は `rgba16uint` に置く。0..=16384 は u16 に収まり、整数で、変換もフィルタもなく、`textureLoad` だけで読み、今の M5 の形を保つ。パック u32 のバッファは、大きな書き換えで決定性の利得がないので採らない。#24 の文言は「最終のタイル出力は `rgba8uint` かパック u32。中間値は Q14 の整数で `rgba16uint` を許す。float の中間は v3 のどこにも置かない」に改訂する。改訂は v3 composite の実装 PR で入る（「値の置き場と順番」）。
2. **color-adjust の v3 の危険。決定済み。** 問題は結びより大きい。`q14_to_f16_bits` は q >= 2048 で 2、4、8 の倍数に丸めるので、#22 の color-adjust の v3 は真の Q14 ではなく、Q14 と名乗る f16 の精度落ちである。20 ケースの一致は結びを踏まず、精度落ちが CPU 参照と一致しただけである。対応は「実装順」の (a) から (c) で、(a) で 8192 を生む画素の数をバックエンドごとに数え、(c) で中間を `rgba16uint` にして f16 の段階と unorm 解決を v3 から除き、color-adjust の v3 を enforce し直し、不透明度 0.5 の parity ケースを足す。
3. **モード 7 の意味。決定済み。** v3 の調整レイヤーでは、screen 以外の非 normal のブレンドはすべて multiply で描く。これを仕様として凍結する（「v3 の範囲：モード 22 と 23」）。
4. **マスクつきグループのポップ。** M5（プレマルチプライに `F`）と #24 の合成パス（ストレート化してからアルファに不透明度）は、半透明のグループで丸めが違い得る。経路を一本にするか、この差を許すかは決めていない。
5. **ブラウザの参照のマスクバイト。** wasm に R8 のマスクタイルを直接読む API はない（`readTile` は表示タイル）。この文書は、JS が矩形の feather の式を作り直すとした。エンジンのコードを二重に持つことになる。読み出し API を足すほうがよいかは決めていない。
6. **マスク生成の決定性。** `hypot`（楕円、投げ縄）、native と wasm の libm、GPU のブラシは、同じ編集を二つのピアが別々に行うと違うバイトを作り得る。内容ハッシュで複製するので、複製後の分岐はない。`render_version` の範囲外だが、どこかで扱う必要がある。
7. **ベクターのカバレッジ。** モード 11 と 12 の乗算は定義したが、カバレッジの Q14 は vector-fill と gaussian-blur の v3 待ちである。feather つきのベクターマスクは両方に依存する。
8. **整数で引けない位置。** v3 のマスクに f32 の位置はない。負のレベルや整数でない `Roi`、Q16 に入らない逆変換は描画を拒否する（DQ3-4）。これらが実際に現れるかは調べていない。現れるなら、拒否をやめるには整数の位置の定義を広げる必要がある。
9. **欠けたスロット。** `0xffffffff` のスロットはカバレッジ 255 になる（v2 の `(1, 0, 0.2, 1)` の `.r`）。デバッグ用の印がマスクとしては「全部見せる」になる。v3 で 0 にするかは決めていない。
10. **古いメモの指し先。** [正確な CPU 参照](2026-10-27-exact-cpu-references.md) の「mask」節は、この文書で置き換えた。あの節からこの文書への指し先は、実装 PR で足す。この変更は文書を一つ足すだけで、既存の文書は触らない。
