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
- **モード 23。normal 以外のブレンドの調整レイヤー。** v2 のモード 7 にあたる。v3 で描くのは、有効なピクセルマスクを持つ color-adjust の調整レイヤーだけである（ブレンドは任意）。式は上の「モード 7」である。マスクのない（または `mask_enabled` が偽の）normal 以外のブレンドは、v3 では拒否する。`v3_gap` / `sceneGap` は v3-unsupported、ホストは描画前に GpuUnsupported とし、float では描かない（#37、Linus 21:46）。float purge で上流のモードは整数になったが、この拒否を外すのは範囲の変更なので、別のレビューで扱う。

**v3 では、screen 以外のブレンドはすべて multiply で描く。** モード 23 の分岐は screen（2）と multiply の二つだけで、overlay、darken、lighten などのほかのモードも multiply の式を通る。これは v2 のモード 7 の意味をそのまま保ったもので、仕様として凍結する。v3 の調整レイヤーで、ブレンドモードごとの式を後から入れることはしない。入れるなら、新しい描画バージョンの変更として扱う。

## マスクのバイトを読む

アトラスは #24 の裁定どおり `Rgba8Unorm` 一枚のままにする。マスク用の `Rgba8Uint` は作らない。

- `textureLoad` で読み、`.r` だけを `unorm_byte` でバイトに戻す。G、B、A は読まない。`textureSample`、サンプラ、ハードウェアのフィルタは使わない。
- 最近傍だけである。v2 の `sparse` も最近傍なので、意味は変わらない。v3 でマスクに 4 タップの補間を入れない。
- タイルの探し方は `sparse` と同じ二分探索である。マップにないタイルはバイト 0、スロットが `0xffffffff` のタイルは v2 と同じ `(1, 0, 0.2, 1)` で、`.r` はバイト 255 になる。
- その後はすべて整数である。`unorm_byte` の往復は `Gpu::atlas_read` と `measureAtlasRead` が 256 値の全チャネルで必須の検査として押さえる。`.r` はそのチャネルの一つなので、新しいアトラスの検査は足さない。

引く位置は常に整数である。ホストの `mask_sampler`（`crates/alto-gpu/src/renderer.rs`）がマスクの逆変換（`mask_entries` の `transform` の逆）を `integer_inverse` で Q16 にし、`SAMPLE_INTEGER` で引く。軸平行で倍率 1 の逆変換も同じ経路を通る。v3 のマスクに f32 の位置（`sparse`）はない。

アップロードするマスクタイルも同じ整数で選ぶ（#37、`64dbb22`。Linus 04:40 で改めた）。ホストの `exact_reach` は、出力 ROI の四隅の画素をシェーダと同じ式（Q8 の画素中心、パックした Q16 の逆行列、0 から遠い側への丸め）で i128 で正確に評価し、テクセル `q >> 8` をさらに `>> 8` してタイルキーにする。Q24 の和は画素の添字について正確にアフィンで、丸めと算術シフトは単調なので、四隅が ROI の全画素を囲む。余白は足さない。f64 の逆行列で選ぶと、量子化誤差（原点から遠いほど大きい）のぶんだけサンプラが読むタイルを落とす。範囲に入るマスクタイルがなければ何も読まない（拒否しない）。拒否するのは下の「DQ3-4」の条件だけである。float purge で、モード 18 のラスタと整数合成の描画も同じ規則（`v3_reach`）で選ぶ。

- 画素中心は `pixel_q8`、ソース位置は `coord_q8`（Rust `source_q8`、JS `sourceAxis`）。テクセルは Q8 の算術右シフト `>> 8`（`floor_frac_q8` の床）で、負の座標でも床になる。タイルキーはテクセルの `>> 8`、タイル内の位置はテクセルからキー × 256 を引いたものである。重み `fx, fy` は使わない。
- `pixel_q8` は整数の原点と倍率を要る。`Roi`（`m5.rs`）の `x`、`y`、`scale` は f32 である。三つがちょうど整数で倍率が 1 以上であることを要る。
- 整数で表せないマスク（逆変換がない、`integer_inverse` が `None`、整数でない原点、倍率が 1 未満）は、描画を `GpuUnsupported` で拒否する（DQ3-4）。届く画素を正確に計算できないときの拒否は、下の「DQ3-4」の読み替え（Linus 04:40）に従う。float で描くことはしない。CPU 参照も同じ条件で拒否する（Rust `mask_byte_cpu` は `None`、JS `maskByteV3` は例外）。

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

## float purge（v3 デフォルト化の前提）

#37 のあと、v3 のシェーダの読み取り経路（M5 の段、resolve、整数合成）に残っていた f32 を除いた（Linus 21:35 / 21:46 の項目 9〜12、00:33 の整数合成、03:25 の Q16 丸め）。v1 / v2 のシェーダ、量子化、出力、ハッシュは変えない。

**範囲（Linus 03:25）。** シェーダの読み取り経路に f32 は残らない。シェーダが読むのは整数の uniform・コマンド・タイルと、`unorm_byte` でバイトに戻すアトラスのテクセルだけである。ホスト側の変換の合成（`multiply`）、逆行列（`inverse`）、Q16 の生成（`integer_inverse` の入力）は f64 のままで、これは未解決である。別の「v3 integer transform」設計メモと PR で追跡する。v3 の既定化とリリースの前の必須ブロッカーである。この PR は v3 経路が完全に float を含まないとは主張しない。

- **項目 9。モード 8、17、19、20。** f32 の `p` から入力座標を作る `load_a` / `load_b` をやめ、モード 21〜23 と同じ整数の `input_map`（`texel_a` / `texel_b`、負の分子は床除算）で読む。設定も f32 の `settings` スロットではなく整数の `grid[0]` に置く。整数で表せない入力領域は GpuUnsupported。`effects-q14.wgsl` から `load_a`、`load_b`、`sparse_bytes` はなくなった。
- **項目 10。モード 18（ラスタの入口）。** f32 の `p` と f32 の逆行列 `inv0` / `inv1`（f64 から f32 へ丸め、FMA の余地あり）をやめ、マスクと同じ Q16 の逆行列と Q8 の画素中心（`lookup`）で引く。タイルの選別もマスクと同じ `v3_reach`。逆行列が特異なら何も描かない（v1 / v2 の f64 の選別と同じ）。
  - **設計判断：最近傍。** 整数の 4 タップではなく最近傍にした。CPU 参照（Rust `q14_program_cpu`、JS `renderM5V3`）は、そのまま置いたラスタを画素ごとにそのまま写す。最近傍は Q8 の位置 `i * 256 + 128` の床で、その画素そのものになり、参照と全バイト一致する（`v3_raster_lookup_is_nearest_and_copies_as_placed`）。整数合成の 4 タップは同じ位置で重み 128 を持ち、隣のテクセルと半分ずつ混ぜるので一致しない。v2 の M5 の `sparse` も最近傍なので、意味も変わらない。
- **項目 11。resolve のキャンバス判定。** `effects-resolve-q14.wgsl` は f32 の中心で `p < canvas` を比べていた。ホストが i64 で 2 倍した式 `0 <= 2 o + (2 i + 1) s < 2 end` からキャンバス上の画素の矩形 `span` を求め（`resolve_span`）、シェーダは整数の添字を比べるだけにした。タイルの原点もキーから i64 で求め直し、f32 の `Roi` と一致しなければ GpuUnsupported（`integer_origin`）。
- **項目 12。不透明度の量子化。** `composite_opacity_q14` は f32 で `v * 16384 + 0.5` を切り捨てていた。積は正確だが和は正確でなく、`2^-15` の直前の f32（Q14 で `0.5 - 2^-25`）が 0 ではなく 1 になる。f32 のビットから整数で `v * 16384` を 0 から遠い側へ丸める `q14_round` に替えた。color-adjust の `color_adjust_q14` も同じ欠陥があったので同じ関数にした。JS（`q14Round`、`opacityQ14`、`colorAdjustQ14`）も BigInt で同じ式にした。
- **Q16 の丸め（Linus 03:25）。** v3 の `integer_inverse` は f64 を 65536 倍して（2 のべきなので正確）0 から遠い側へ丸める。`(scaled + 0.5).floor()` は和を先に丸めるので、結びのすぐ下で誤る（v = 2^-17 - 2^-70 は 0.5 - 2^-54 で、和が 1 に丸まり 1 になる。正しくは 0）。v3 は `f64::round()`（値そのもので決める正確な half away from zero）に替えた。JS の `quantizeQ16` は `a = |x|`、`f = floor(a)`、`a - f >= 0.5` なら `f + 1`（`a - f` は Sterbenz で正確）で、符号を戻す。`sign * floor(|x| + 0.5)` は同じ誤りを持つので使わない。v2 は `integer_inverse_v2` / `quantizeQ16V2` で従来の丸めを保ち、量子化とハッシュは動かない。検査は `q16_rounding_is_exact_half_away` と Rust・JS の一致（`q16-reference.mjs`）。
- **uniform。** v3 の `Config` は float のフィールドを宣言しない（f32 のスロットは `unused` で、ホストは 0 のままにする）。
- **アトラス。** `Rgba8Unorm` のままにし、`textureLoad` と `unorm_byte` でバイトに戻す。整数形式（`r8uint` / `rgba8uint`）は採らない。アトラスは v1 / v2 と共有の一枚で、WebGPU では `Rgba8Unorm` と `Rgba8Uint` はビュー互換でない。v3 だけを整数にするには、二枚目のアトラス（アップロードとメモリの二重化）か、v1 / v2 のシェーダのアトラスの型の変更が要る。後者は v1 / v2 を一文字も変えないという条件に反する。`unorm_byte` の往復は 256 値の全チャネルで `Gpu::atlas_read` が必須の検査として押さえ、v3 で float の値に触れるのはシェーダの `atlas_bytes` 一か所だけである（`v3_stages_call_no_f32_coordinate_helper`）。

- **整数合成（プレーンな経路、Linus 00:33）。** `integer_composite_shader`（`composite-q14.wgsl`）にも f32 が残っていた。軸平行で倍率 1 の描画の f32 最近傍（`fetch_bytes` の `floor(q)`）、f32 のキャンバス判定、f32 の uniform から読むタイル原点と倍率である。どれも整数にした。
  - 描画ごとにホストの `plain_draw_v3` が、Q16 の逆行列（ないときは GpuUnsupported）、フィルタ、読むタイルキーの範囲を決める。フィルタは Q16 の係数で判定し（`axis_unit_q16`：軸外が 0 から 6 以内、軸上が ±65536 から 6 以内。f32 の `|v| < 0.0001` の整数版）、軸平行で倍率 1 なら Q8 位置の床の最近傍（`SAMPLE_NEAREST`）、それ以外は従来の整数 4 タップである。f32 の取得は v3 にない。
  - タイルの選別と、不透明な上の層が下の層を隠す近道は、キャンバス上の画素の四隅を `pixel_q8` と `source_q8` で評価したキーの範囲で行う（4 タップは隣のテクセルまで）。f64 の逆行列で選ぶと、サンプラが読むタイルを落としたり、読む範囲に穴のある層で下を隠したりし得る。範囲に入るソースのタイルがない描画はスキップする。拒否は下の「DQ3-4」の条件だけである。
  - キャンバス判定は M5 と同じ `resolve_span`、uniform は整数の原点・コマンド数・矩形（`IntegerConfig`）。
  - v3 のハッシュは変わり得る（軸平行で倍率 1 から少しずれた描画の最近傍）。v3 はまだデフォルトでなく、ロック済みの v3 ハッシュはない。v1 / v2 のシェーダとハッシュは変えない。

検査は、モードごとに GPU と CPU の全バイト比較（原点、負のキー、10^6 付近、2^23 の直前、レベル倍率 4、回転・縮小・結びのラスタ、キャンバスの端、レベル 1 のピラミッド、実際の `Renderer::read_tile` 経由）、Rust と JS の一致（`m5-v3-renderer-reference.mjs`）、`main` から届く関数に float がないことの検査である。

### DQ3-4：GpuUnsupported の定義（Linus 04:40）

v3 は、整数で正確に定義できない画素を float で描かず、`GpuUnsupported` で拒否する（DQ3-4）。#37（Linus 22:28）は「タイル範囲を整数で求められなければ拒否」としていた。この定義はレイヤーが描かないタイルでも拒否を起こし、拒否の可否がキャッシュの状態で変わった（同じシーンが、コールドでは GpuUnsupported、damage の外のウォームでは透明）。04:40 に次のとおり読み替えた。

- **GpuUnsupported は、そのタイルに実際に届く画素を、整数で正確に計算できないときだけ。** 届かない画素は整数で透明（マスクはバイト 0）と定義済みなので、拒否しない。
- **選別。** 四隅の Q8 を i128 で正確に評価し、ソースのタイルキーの範囲を求める（`exact_reach`。4 タップは各軸 1 テクセル先まで）。その範囲を、ソース（ラスタ、マスク、ストロークの上書きを含む）の有限なタイルの集合と交差させる（`v3_reach`）。交差が空なら、そのソースはそのタイルに寄与しない。拒否も読み取りもせずスキップする（整数合成はコマンドを出さず、M5 はタイルを上げない）。ホストは i128 で評価するので、i64 の溢れでは拒否しない。シェーダの 64 ビットの和は、真の値が i64 を出るときにしか溢れない。積はそれぞれ 2^62 未満、平行移動は 2^61 未満である。
- **拒否。** 交差が空でないときだけ、次のどちらかで拒否する。
  1. 四隅の Q8 の画素中心が i32 に入らない（タイル原点が ±2^23 px の外。「plain [32768, 0] refused」「renderer [32768, 0] raster refused」）。
  2. 四隅のソースの Q8 が i32 を出る軸がある（シェーダは、真の Q8 が i32 を出る画素をちょうど落とす。`ok == 0` で透明）。そのうえで、範囲に入るタイルがテクセル `-2^23 + reach ..= 2^23 - 1` に収まらない。収まれば、落ちた画素のテクセルは `2^23` 以上か `-2^23 - 1 + reach` 以下なので、どのタイルも読まない。i32 の溢れが、届く画素を消すことはない。収まらなければ、そのタイルだけ拒否する（「mask tile at 2^23 refused」。キー 32767 だけなら「mask up to 2^23 draws」で描く）。
- 逆行列が Q16 に入らない描画（`integer_inverse` が `None`）は、届く範囲を求められないので、どのタイルでも拒否する。
- **damage。** v3 のキャッシュの無効化も、同じ整数の範囲で求める。`scene_damage` は v3 で `Damage::Sources` を返す。f64 の矩形のほかに、変わった描画ごとにワールド変換と、変わったソースのタイルのキーの箱を持つ。`invalidate_reach` は、キャッシュの各タイル（全レベル。レベル 0 の画素の全体）の `exact_reach` がその箱と交わるものを捨てる。ストロークのプレビューも、触れたタイルの箱で同じことをする。拒否もバイトも、届く描画だけで決まるので、捨てなかったタイルは変わらない。Q16 の逆行列のない描画が変わったら全部捨てる。M5 のシーンは従来どおり全部捨てる（`Damage::All`）。
- **検査。** `v3_reach_refuses_only_what_reaches_a_tile`（スキップと拒否の境界。2^23 の両側、最近傍と 4 タップの負の側）。`renderer_cold_warm`（`gpu_shader_matches_cpu_reference` の中）は、同じシーンを、変更前のタイルをキャッシュして damage を当てたウォームと、キャッシュを空にしたコールドで読み、全バイト一致（と CPU 一致）か同じ拒否であることを確かめる。対象は large-translation-out-of-range、2^23 の境界（描く、拒否、遠いレイヤー）、動かしたレイヤー（レベル 0 と 1）、999936 の端でのタイル追加、マスクつき M5 である。ブラウザの parity は、v3 の各ケースでウォームとコールドの全バイト一致を確かめる。large-translation-out-of-range は v3 でも透明で CPU と一致する。

### 未解決：v3 integer transform（v3 既定化・リリース前の必須ブロッカー）

シェーダに渡る Q16 の逆行列と平行移動は、ホストが f64 で作る。ノードの変換の合成（`renderer::multiply`、`Affine2::concat`）、逆行列（`renderer::inverse`、`Affine2::inverse` の除算）、リンクしたマスクの変換の合成は f64 の算術で、その結果を `integer_inverse` が丸める。f64 の演算は IEEE で一意で、Rust と JS は同じ式を同じ順で評価するので現在の検査では一致するが、整数で定義された変換ではない。これを整数（または有理数）の変換として定義し直すのは、別の「v3 integer transform」設計メモと PR で扱う。
