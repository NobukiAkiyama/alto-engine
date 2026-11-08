# 追加効果

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。追加の経路は同書の「効果と描画の追加手順」。

**Goal:** シャープ、移動ぼかし、ノイズ、ハイパス、ドロップシャドウ、境界線、自動補正、非破壊グラデーションを既存の効果スタックへ載せる。

**Architecture:** 効果は順序付きの `Vec<Effect>`。`Effect` に variant を足し、Q14 の段を追加する。入力より広がる効果（シャドウ、境界線）は、現在の ROI 逆伝播だけでは足りず、出力範囲の伝播を新設する。

## 現状

- `Effect`：`GaussianBlur`、`ColorAdjust`、`Levels`、`HueSaturation`、`Invert`、`Curves`。どれも `selection: Option<TileMap>` と `extra` を持つ。
- `m5.rs` の `input_roi_v3` が出力から入力の範囲を逆算する。出力が入力より広がる効果はない。
- ノード当たり 32 個、ぼかし半径は 0〜64 文書px、半径の単位はレイヤー変形後の文書座標。
- Q14 のブラーは `blur_stage`（モード 24）。

## 共通の文書形式

全 variant に既存と同じ `selection` と `extra` を持たせる。

| type | フィールド | 範囲 | 既定 |
|---|---|---|---|
| `unsharp_mask` | `radius`、`amount`、`threshold` | 0〜64、0〜5、0〜255 の整数 | 2、1、0 |
| `motion_blur` | `angle`、`distance` | −180〜180 度、0〜128 | 0、8 |
| `noise` | `amount`、`seed`、`monochrome` | 0〜1、u32、bool | 0.1、0、true |
| `high_pass` | `radius` | 0.5〜64 | 4 |
| `drop_shadow` | `offset_x`、`offset_y`、`blur`、`spread`、`color`、`opacity` | ±4096、0〜64、0〜64、RGBA、0〜1 | 4、4、8、0、黒、0.75 |
| `stroke_effect` | `width`、`position`、`color`、`opacity` | 0〜64、`outside` / `inside` / `center`、RGBA、0〜1 | 2、`outside`、黒、1 |
| `gradient_overlay` | `gradient`、`opacity`、`blend` | 下記、0〜1、`BlendMode` | 必須、1、normal |

`gradient` は `{ kind: "linear" | "radial", from: [x, y], to: [x, y], stops: [{ at, color }] }`。stops は 2〜16 個、`at` は狭義単調増加。座標はノード座標。

## FX-4 ハイパス

**式。** Q14 の straight 値で `out = clamp(x − blur(x) + 8192)`。アルファはそのまま。ブラー段（モード 24）の出力を B に、元を A にして新モード 34 で結合する。ROI はぼかしと同じ。

## FX-1 シャープ（アンシャープマスク）

**式。** Q14 の straight 値で `d = x − blur(x)`、`|d| ≤ threshold × 64` なら `out = x`、それ以外は `out = clamp(x + (amount_q8 × d + 128) >> 8)`。結合段はモード 35。量子化はホスト側で行い `amount_q8 = round(amount × 256)`。アルファは変更しない。

## FX-2 移動ぼかし

**設計。** 角度方向の一次元の箱平均。`taps = 2 × ceil(distance / 2) + 1`（最大 129）。

- ホストが `cos`、`sin` を Q16 の表（1/256 度刻みの CORDIC）で求め、各タップの整数オフセット `(dx_i, dy_i)` を丸めて uniform に格納する。文書の f64 は量子化前に `validate_effect` で範囲を確かめる。
- 拡大縮小時のレベル変換は、既存のぼかしと同じく文書に固定した粗いグリッド（`coarsen`）を使う。
- premultiplied Q14 の和を `taps` で割る。丸めは 0.5 を足して切り捨て。
- ROI：`expand(ceil(distance / 2), ceil(distance / 2))`。

## FX-3 ノイズ

**設計。** 文書画素ごとの決定的なハッシュ。座標は文書座標、タイルや表示レベルに依存しない。

- `h = pcg32(x_doc as u32 ^ pcg32(y_doc as u32 ^ seed))`。整数のみで WGSL、Rust、JS で一致する。
- 単色は同じ値を全チャネルに加算、カラーはチャネルごとに `pcg32(h ^ channel)`。
- 値域を `[−amount_q14, amount_q14]` に写し、straight 値へ加算して clamp。アルファは変更しない。
- 拡大時はレベル0の文書画素の値を最近傍で参照する。v3 はレベル1以上を子の縮小から作るので、縮小表示でノイズは平均される。

## FX-5 ドロップシャドウ

**設計。** 入力のアルファから影を作り、入力の背面に合成する。

1. 入力のアルファを `spread` で膨張（FX-6 の距離段を共用）。
2. `blur` で既存のブラー段を通す。
3. `(offset_x, offset_y)` だけ整数移動。ホストが丸める。
4. 色と `opacity` を掛け、入力を上に Normal で合成（モード 36）。

**出力範囲の伝播（本効果で新設）。**

- `alto-scene` に `fn effect_output_bounds(effects: &[REffect], bounds: IRect) -> IRect` を追加し、変更範囲と描画範囲の両方に使う。編集の差分描画（`alto-scene` の変更範囲）と `m5.rs` の `input_roi_v3` の双方で同じ関数を基準にする。
- シーンの範囲判定でタイルの可視判定が効果の外へ広がる。ノードの範囲が画面外でも影が画面内に入る場合に描かれること。
- 親グループの範囲にも伝播する。

## FX-6 境界線

**設計。** アルファの距離場を作る。Jump Flood（JFA）を `ceil(log2(width + 1))` 回行い、整数のユークリッド距離の二乗を比較する。誤差は既知の少数の画素に出るが、3実装で同じ結果になる。

- `outside`：距離 ≤ width の領域を塗り、入力を上に合成。
- `inside`：入力の補集合の距離を使う。
- `center`：両方を width/2 で適用。
- 端の被覆：距離と width の差を 1 文書px で線形に落とし、Q14 で量子化。距離の平方根は整数ニュートン法で固定回数（4回）。

**判定条件。** width 64 で 4K レイヤーの段の時間を測り、1フレーム予算を超える場合は `outside` のみ先行する。

## FX-7 自動補正

**設計。** 効果の種類は増やさない。`doc.image.histogram` を使う SDK の補助で、`Levels` の引数を計算する。

`doc.effects.autoLevels(layer, { clip?: number }): Promise<LevelsParams>` は、各チャネルの全ヒストグラムから下端と上端を `clip`（既定 0.001）で切り、`in_black`、`in_white` を返す。適用はしない。決定的で文書に自動値は残らない。

## FX-8 複数入力の効果

**設計。** 置き換えマップのように2枚を読む効果。`Vec<Effect>` の直列では表せない。

`EffectGraph { nodes, edges }` へ広げる案を設計書 3.7 が想定している。現状の用途は置き換えマップのみで、「別のレイヤーを参照する効果」は依存関係の変更範囲、ROI、GC の根を全部変える。そこでグラフにせず、効果の引数に `map: AssetId` を持たせ、アセット画像に限定する（ノード参照なし）。グラフへの拡張は別の用途が出てから行う。

- `displacement_map`：`map`（RGBA8 の `EncodedImage` を取り込み時に専用タイルへ）、`scale_x`、`scale_y`（±128）、`wrap`（`repeat` / `clamp`）。
- 変位は `(R − 128) × scale_x / 128`、`(G − 128) × scale_y / 128` の整数画素。読み出しは最近傍で、3実装を一致させる。
- ROI：`expand(|scale_x|, |scale_y|)`。出力範囲は入力と同じ。
- `map` の参照を GC の根と差分に含める。使用中のアセットの削除は拒否する。

## FX-9 非破壊グラデーション

**設計。** `gradient_overlay` 効果とする。ラスター確定の `gradient`（`image.rs`）とは別に、Q14 の評価段（モード 37）を追加する。

- 勾配の位置 `t` は Q16。`from`、`to` はホストがノード座標から出力レベルの固定小数へ写す（`FixedAffine`）。放射はニュートン法の整数平方根。
- 色は Q14 の straight 値を区分線形で補間し、stops を uniform へ格納する。
- ベクター塗り（VEC-1）も同じ関数を WGSL の共通ファイルから使う。

## 実装タスク

各タスクは同じ構成である。Files は 00 の「効果と描画の追加手順」の全段。Consumes は既存の `blur_stage`、`input_roi_v3`。

### Task FX-4 / FX-1: ハイパスとシャープ

**Interfaces:** Produces `Effect::HighPass { radius, selection, extra }`、`Effect::UnsharpMask { radius, amount, threshold, selection, extra }`、`REffectKind::HighPass { radius_q8 }`、`REffectKind::UnsharpMask { radius_q8, amount_q8, threshold }`、モード 34、35。

- [ ] 試験 `validate_rejects_unsharp_amount_above_5` と `validate_rejects_high_pass_radius_below_half`。
- [ ] CPU 参照の試験 `high_pass_of_flat_color_is_mid_gray`（単色入力で RGB が 8192、アルファ維持）。
- [ ] CPU 参照の試験 `unsharp_zero_amount_is_identity`、`unsharp_threshold_keeps_small_differences`。
- [ ] WGSL、JS を実装し、parity ケース `effect-high-pass`、`effect-unsharp` を `tests/parity/` へ追加。
- [ ] `node scripts/verify-browser.mjs --functional` の parity が成功することを確認。
- [ ] 既存の行が1行も変わっていないことを `git diff tests/parity` で確認して PR。

### Task FX-2: 移動ぼかし

**Interfaces:** `Effect::MotionBlur { angle, distance, selection, extra }`、`REffectKind::MotionBlur { dx_q16, dy_q16, taps }`、モード 38。

- [ ] 試験 `motion_blur_zero_distance_is_identity`、`motion_blur_of_flat_color_is_flat`。
- [ ] 試験 `motion_blur_roi_expands_by_half_distance`（`input_roi_v3`）。
- [ ] タイル境界をまたぐ parity ケース `effect-motion-blur-tile-seam`。
- [ ] 実装、parity 追加、PR。

### Task FX-3: ノイズ

**Interfaces:** `Effect::Noise { amount, seed, monochrome, selection, extra }`、モード 39、共通 `pcg32(u32) -> u32`（Rust、WGSL、JS）。

- [ ] 試験 `pcg32_matches_known_vectors`（3実装で同じ既知ベクトル）。
- [ ] 試験 `noise_is_independent_of_tile_origin`（同じ文書画素が異なるタイル分割で同じ値）。
- [ ] 試験 `noise_zero_amount_is_identity`、`noise_keeps_alpha`。
- [ ] 実装、parity 追加、PR。

### Task FX-5: ドロップシャドウと出力範囲

**Interfaces:** Produces `effect_output_bounds(effects: &[REffect], bounds: IRect) -> IRect`（`alto-scene`）、`Effect::DropShadow`、モード 36。

- [ ] 試験 `output_bounds_grow_by_offset_blur_and_spread`。
- [ ] 試験 `editing_a_shadowed_layer_invalidates_the_shadow_area`（変更範囲に影の範囲が入る）。
- [ ] ブラウザ試験 `a shadow from an offscreen layer is drawn`：ノードが画面外、影が画面内。
- [ ] 試験 `group_bounds_include_child_shadow`。
- [ ] 実装、parity 追加、PR。

### Task FX-6: 境界線

**Interfaces:** Consumes `effect_output_bounds`。Produces `Effect::StrokeEffect`、JFA のモード 40、結合のモード 41。

- [ ] 試験 `stroke_outside_keeps_the_input_above`、`stroke_inside_stays_within_alpha`、`stroke_zero_width_is_identity`。
- [ ] parity ケース `effect-stroke-outside`、`-inside`、`-center`。
- [ ] Metal で width 64 の段時間を測り、判定条件を記録。
- [ ] 実装、PR。

### Task FX-7: 自動補正

**Files:** Modify `packages/alto-sdk/src/document.ts`、`packages/alto-sdk/api`。

**Interfaces:** `doc.effects.autoLevels(layer, options?)`。

- [ ] Node 試験 `autoLevels clips the histogram ends`、`autoLevels on a flat image returns the identity range`（`in_black < in_white` を保つ）。
- [ ] 実装、PR。

### Task FX-9: 非破壊グラデーション

**Interfaces:** Produces 共通シェーダー関数 `gradient_q14(t_q16, stops) -> vec4i`（VEC-1 が再利用）、`Effect::GradientOverlay`、モード 37。

- [ ] 試験 `stops_must_increase`、`gradient_two_equal_stops_is_flat`。
- [ ] 試験 `radial_center_equals_first_stop`。
- [ ] parity ケース `effect-gradient-linear`、`-radial`、`-tile-seam`。
- [ ] 実装、PR。

### Task FX-8: 置き換えマップ

**Interfaces:** `Effect::DisplacementMap { map, scale_x, scale_y, wrap, selection, extra }`、モード 42。

- [ ] 試験 `neutral_map_is_identity`（全画素 128）、`removing_a_map_in_use_is_refused`、`gc_keeps_the_map`。
- [ ] 試験 `missing_map_asset_is_refused_atomically`。
- [ ] parity ケース `effect-displacement`、`-tile-seam`、`-repeat`。
- [ ] GPU 喪失後にマップがアセットから再構築されることを試験。
- [ ] 実装、PR。

## Review Focus

- 効果を持つレイヤーの Undo と変更範囲で、効果の広がった部分に古い絵が残らないこと。
- レイヤー変形（回転、拡大）を挟んだ効果の半径が、既存のぼかしと同じ文書座標の単位であること。
- `selection` 付きで、効果の外側が元の画素のままであること。
- 32個の上限でスタックしたときに GPU 予算超過が明示エラーになること。
- `canvas.resize` と `canvas.rotate` で新しい効果の引数がどう変わるか。`canvas.rs` は半径の拡大が 64 を超えると拒否するので、同じ検査を全引数へ入れる。
