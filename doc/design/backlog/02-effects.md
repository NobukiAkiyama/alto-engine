# 追加効果

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。追加の経路は本書の「追加の経路」（00 の「効果と描画の追加手順」は本書に合わせた要約。詳細は本書を正とする）。

**Goal:** シャープ、移動ぼかし、ノイズ、ハイパス、ドロップシャドウ、境界線、自動補正、置き換えマップ、非破壊グラデーションを既存の効果スタックへ載せる。

**Architecture:** 効果は順序付きの `Vec<Effect>`（ノード当たり 32 個まで）。`Effect` に variant を足し、Q14 の段（`effects-q14.wgsl` のモード）を追加する。文書が持てる描画版は v3 だけで（`DocumentMeta::validate` が v3 以外を拒否する。レンダラーは v1・v2 の float 段もコードに残す）、新しい効果は v3 の整数段だけを持つ。ぼかしを内部に使う効果は、既存のぼかしと同じく「文書に固定した粗いグリッド」で平面を作ってから合成する。入力より広がる効果（シャドウ、境界線、移動ぼかし）は、ROI の逆算を方向つきの広がり量へ広げ、順方向の出力範囲 `effect_output_bounds` を 05 の IMG-1 へ提供する。（新規）

## 共通の現状

- 文書の型：`crates/alto-doc/src/effect.rs` の `Effect`（`GaussianBlur`、`ColorAdjust`、`Levels`、`HueSaturation`、`Invert`、`Curves`）。どれも `selection: Option<TileMap>` と `extra` を持つ。JSON は手書きの `Serialize` / `Deserialize` で、`effect_from_map` が既知のキーを取り除き、残りを `extra` に入れる。`Effect::validate` は `extra` が空でなければ `unknown effect fields`（`UNSUPPORTED` 相当の `DocError::Unsupported`）で拒否する。`Effect::selection`、`Effect::selection_mut`、`Effect::validate`、`Serialize`、`effect_from_map` の5か所が variant ごとの match を持つ。
- 上限：ノード当たり 32 個（`crates/alto-doc/src/node.rs` の `LayerProps::validate`、`At most 32 effects per node`。`Engine::effect_ops` も同じ検査）。ぼかし半径 0〜64 文書 px。範囲外は `DocError::Invalid`（`INVALID_ARGUMENT`）。調整レイヤー（`NodeKind::Adjustment { adjustment }`）は効果を1つ持ち、`Document::validate` が `validate_effect` を呼ぶ。
- 描画用の型への変換：`crates/alto-engine/src/selection.rs` の `render_effect`（`alto_doc::Effect` → `REffect`）。ぼかしの半径だけは f64 のビット列から Q8 を直接求める（`alto_core::blur::blur_radius_q8_f64`。f32 に丸めると別の半径が同じ値になるため）。他の数値は `as f32` で渡し、ホストが Q14 へ量子化する。効果の選択範囲のタイルは `effect_hashes` が GC の根にする。`crates/alto-scene/src/lib.rs` の `REffectKind` は6種類で、`alto-scene` は GPU に依存しない。
- v3 の採否：`crates/alto-gpu/src/renderer/geometry.rs` の `v3_gap` の `integer_effect` に無い種類は、シーンごと拒否される（`effects`、`adjustment`）。JS の鏡は `tests/e2e/cpu-composite.js` の `isIntegerEffect` と `sceneGap`（`v3-unsupported`）。`Context::stage`（`crates/alto-gpu/src/renderer/tile_renderer/stages.rs`）は、v3 で整数段を持たない種類が届くと `Internal` を返す。v1・v2 の float 段（モード 0〜16）は文書から到達できない。
- 段の組み立て：`Context::stage` が1効果を1回以上の dispatch にする（v3 のぼかしは水平・垂直の2回）。`selection` があれば、さらにモード 22 の1回で元と混ぜる（選択つきぼかしは3回）。`Context::stack` がスタックの最後から再帰し、**最後の効果がぼかしのときだけ**、`blur_coarsen_k` で決めた粗いレベルで平面を作り（`RoiQ::coarsen`）、`crop`（モード 25 の再標本）で `desired` へ戻す。`Context::effects` は調整レイヤー用で、ぼかしを含むときだけ `graph.rs` の `Context::children` が `stack`（`StackInput::Siblings`）を使う。ROI の逆算は `Context::input_roi_v3` と `Context::effects` の中の2か所にあり、どちらもぼかしだけを広げる（`RoiQ::taps`、`RoiQ::expand`）。レベル 0 の広がりは `ceil(radius_q8 / 256)` 画素。
- 描画の単位：表示タイルごとに `compose_tiles` が、シーン全体を `desired` の ROI で根から再帰して描く。ノードの範囲による除外は無い（`lookup_entries` がタイルの届く範囲だけを読む）。v3 のレベル 1 以上は子の縮小で作る（`compose_from_children`）ので、効果の段が直接走るのはレベル 0 以下（拡大側、最小 −6）である。
- 変更範囲：`crates/alto-scene/src/damage.rs` の `scene_damage` は、効果・マスク・クリップを1つでも含むシーンを `plain_scene` でないとして `Damage::All` にする。つまり効果つきのレイヤーの編集は、いま常に全タイルを捨てる。効果の広がりを変更範囲へ入れる必要は、いまは無い。
- 既存の注意（読んで確かめた）：`Context::stack` のぼかしの枝は、`selection` があると `previous` を2回評価する（粗い平面用と、`original` 用）。選択つきのぼかしが n 個連なると 2^n 回になる。新しい効果の多くが同じ枝を使うので、FX-4 の作業手順で先に直す。
- 参照実装と固定：ぼかしだけが全バイト一致の独立参照を持つ（Rust：`crates/alto-gpu/src/cpu_composite/v3_tests.rs` の `blur_pass_reference`、JS：`tests/e2e/cpu-composite.js` の `renderBlurV3`、`blurPassQ14`、`blurKernelQ16`）。`color_adjust` は `crates/alto-gpu/src/cpu_composite/color_adjust.rs` の格子と `tests/parity/cpu-reference.txt`。`levels`、`curves`、`hue_saturation`、`invert` は既知の値を許容差つきで確かめるだけで（`tests/e2e/parity-tests.js` の `v3-*-effect`）、独立参照を持たない。**新しい効果はすべて、ぼかしと同じ全バイト一致の独立参照（Rust の整数、JS の BigInt）を持つ**（共通規則2）。ブラウザーの parity 行は `tests/parity/v3-browser.txt` に追記する。
- GPU の予算：1つの段は、ROI の各辺を `bucket` で丸めた大きさ × 8 バイトの目標（`rgba16uint`）を、同じ大きさで未使用のものがなければ確保する（`Context::alloc`、プールの再利用）。予算の判定は、保留中・廃棄待ちの確保を含む全体の使用量との合計で行う。予算を超えると `RenderError::Budget`（コードは `INVALID_ARGUMENT`、文面は `effects exceed GPU memory budget`）。ROI がテクスチャの最大辺を超えると `effect ROI exceeds texture limits`。どちらも描画時のエラーで、編集は止めない。
- SDK：`packages/alto-sdk/src/types.ts` の `Effect`（6種類の共用体と `EffectScope`）、`doc.effects.set`（`packages/alto-sdk/src/document.ts`）。Worker の要求は `setEffects`（`packages/alto-sdk/src/protocol.ts`、`effects: unknown[]`）なので、種類が増えても要求の形は変わらない。ヒストグラムは `doc.image.histogram`（`imageHistogram`、`Engine::image_histogram`）。
- キャンバス編集：`crates/alto-engine/src/canvas.rs` の `scale_effects` が、`canvas.resize` のときだけ `GaussianBlur` の半径を拡大率で掛ける。非一様な拡大は `UNSUPPORTED`、拡大後の半径が 64 を超えても `UNSUPPORTED`。`canvas.rotate`、`canvas.flip` は効果に触れない（半径は回転で変わらないため）。

## 共通の設計

### 追加の経路

新しい効果は次の全段を通す（00 の表の `m5.rs` などは改名・分割済み）。

| 段 | 場所 |
|---|---|
| 文書の型と検証 | `crates/alto-doc/src/effect.rs`：`Effect`、`effect_from_map`、`Serialize`、`selection`、`selection_mut`、`validate`。キーの取り出しに `effect_u32`、`effect_bool`、`effect_color`（新規）を足す。ノード当たりの個数の検査は `crates/alto-doc/src/node.rs` |
| 描画用の型 | `crates/alto-engine/src/selection.rs`：`render_effect`、`effect_hashes`。`crates/alto-scene/src/lib.rs`：`REffectKind` |
| v3 の採否 | `crates/alto-gpu/src/renderer/geometry.rs` の `v3_gap`、`tests/e2e/cpu-composite.js` の `isIntegerEffect` |
| GPU の段 | `crates/alto-gpu/src/renderer/tile_renderer/stages.rs`（`stage`、`stack`、`effects`、`input_roi_v3`）、`graph.rs`（調整レイヤー）、`params.rs`（`q14_params`、`blur_params`） |
| シェーダー | `crates/alto-gpu/shaders/effects-q14.wgsl` の `main` の分岐。連結順は `integer_effects_shader`（`crates/alto-gpu/src/lib.rs`）。使用中のモードは 8、17〜33（ほかのシェーダーは別の番号を持つ）。新規は 34〜43（下の割り当て。00-ROADMAP の表）。モード 6 は v3 で分岐が無く 0 を書く（`targets.rs` の `zero`）ので使わない |
| float の禁止 | `crates/alto-gpu/src/renderer/tile_renderer/pool_tests.rs` の `v3_stages_call_no_f32_coordinate_helper` が、`main` から届く全関数の float トークンを禁じる。新しい段もこれに通る |
| CPU 参照 | Rust：`crates/alto-gpu/src/cpu_composite/effect_reference.rs`（新規。整数の独立実装と既知の値）。GPU との比較は `v3_tests.rs` の流儀。JS：`tests/e2e/cpu-composite.js` に BigInt で独立に実装（新規） |
| parity | `tests/e2e/parity-tests.js` に症例、`tests/parity/v3-browser.txt` に行（`ALTO_WRITE_V3_BROWSER=1 node scripts/verify-browser.mjs --suite=parity`。追記のみ）。`tests/parity/cpu-reference.txt` は render v2 の合成の固定なので触れない |
| SDK | `packages/alto-sdk/src/types.ts` の `Effect`、`packages/alto-sdk/api/alto-engine.api.md`（`pnpm --dir packages/alto-sdk api`） |
| キャンバス編集 | `crates/alto-engine/src/canvas.rs` の `scale_effects`（下の表） |

モードの割り当て：本書の新規モードは次の10個。34 ハイパスの結合、35 シャープの結合、36 移動ぼかし、37 ノイズ、38 距離場の初期化、39 距離場の1歩、40 距離から被覆、41 色つき平面、42 勾配の平面、43 置き換え。番号は段の働きに固定し、PR の順が変わっても変えない。結合の合成には既存のモード 19（`layer_q14`：dst = A、src = B、不透明度、合成モード）、27（`clip_layer_q14`：基底のアルファを保つ）、22（選択）、25（再標本）、24（ぼかし）を使う。

### ぼかしを内部に使う効果（FX-1、FX-2、FX-4、FX-5、FX-6）

`Context::stack` の「最後の効果がぼかし」の枝を、次の述語で一般化する（実装は FX-4 の作業手順）。

- `fn smoothing_radius_q8(kind: &REffectKind) -> Option<u32>`（新規。置き場は `stages.rs`）：`GaussianBlur` は `radius_q8`、`HighPass` と `UnsharpMask` は `radius_q8`、`MotionBlur` は `distance_q8 / 2`、`DropShadow` は `max(blur_q8, spread_q8)`、`StrokeEffect` は `width_q8`（`center` は半分）。それ以外は `None`。値は 16384（64 文書 px）以下で、`RoiQ::taps` の上限と同じ。（新規）
- 粗いレベルは `blur_coarsen_k` と同じ規則：`max(desired のレベル, k)`。`k` は半径 ≤ 64 · 2^k を満たす最小の整数で、半径 ≤ 64 のとき k ≤ 0 である。平面は粗いレベルで作り、`crop` で `desired` へ戻し、結合は `desired` のレベルで行う。
- 結合する効果（ハイパス、シャープ、シャドウ、境界線）は、「元」を `desired` で、「平面」を粗いレベルで作る。元の評価はぼかしの `selection` の枝（`original`）と同じ形である。
- 調整レイヤーの判定（`graph.rs` の `Context::children` にある `GaussianBlur` かつ半径 > 0 の検査）も、`smoothing_radius_q8` が `Some` で 0 でない、または「広がる効果」（下の `effect_growth_q8` が 0 でない）に置き換える。広がる効果は兄弟の再評価（`StackInput::Siblings`）が要るためである。（新規）

### 広がり量と ROI（入力より広い効果）

`alto-scene` に、効果1つが出力を入力の外へ広げる量を返す関数を置く。

```rust
/// レベル 0 の文書画素、Q8。[左, 上, 右, 下] へ出力の範囲が入力の範囲より広がる量。（新規）
pub fn effect_growth_q8(kind: &REffectKind) -> Option<[i64; 4]>;
/// 順方向：入力の範囲 bounds（文書画素）に effects を順に掛けた出力の範囲。桁あふれは None。（新規）
pub fn effect_output_bounds(effects: &[REffect], bounds: IRect) -> Option<IRect>;
```

- `effect_growth_q8`：色の効果、ノイズ、ハイパス、シャープ、勾配は `[0; 4]`（アルファを変えない）。`GaussianBlur` は `ceil(radius_q8 / 256) * 256` を4辺に。`MotionBlur` は `ceil((distance_q8 / 2) / 256) * 256` を4辺に（角度によらない上界）。`DropShadow` は `g = blur_q8 + spread_q8` として、左 `max(0, g - ox*256)`、右 `max(0, g + ox*256)`、上下も同様（シャドウは入力と重なるので、合併の外側だけを数える）。`StrokeEffect` は `outside` が `width_q8`、`center` が `width_q8 / 2`、`inside` が 0 を切り上げて4辺に。`DisplacementMap` は `ceil(|scale|)` を軸ごとに。（新規）
- 値は `RoiQ::taps` が使う切り上げ（`ceil(q8 * 2^-level / 256)` 画素）と同じ式でレベルの格子へ写す。したがって `input_roi_v3` と `effect_output_bounds` は同じ Q8 から出る。（新規）
- `Context::input_roi_v3` と `Context::effects` の ROI 逆算は、`effect_growth_q8` を `RoiQ::expand` の左右上下の4値へ写す。`RoiQ::expand` は対称（`x`、`y` の2値）なので、方向つきの拡張を持つ `RoiQ::grow(left, top, right, bottom)`（新規）を足す。逆算では、出力の広がりの左右・上下を入れ替えた量（シャドウは `ox` の符号が逆）で入力の ROI を広げる。
- 結果は箱の合併（外接矩形）にする。移動量が大きいと入力の ROI が大きくなるので、`DropShadow` の移動量が格子で 64 画素を超えるときは、影の平面だけを移動した ROI で別に評価してよい（結果は同じ）。評価の重複が 2 倍に増えるので、`drop_shadow` はノード当たり 4 個までに制限する（`MAX_DROP_SHADOWS_PER_NODE = 4`（新規）、超えると `INVALID_ARGUMENT`）。
- 使い道：05 の IMG-1（焼き込みの範囲 = ノードの外接矩形 ∪ 出力範囲）。効果つきシーンは `Damage::All` なので、変更範囲には使わない。将来、効果つきシーンの差分描画を作るときは同じ関数を使う。

### 調整レイヤーに許す効果

調整レイヤーは背後（アキュムレータ）に効果を掛ける。`drop_shadow` と `stroke_effect` は背後の不透明度の輪郭に影・線を描くことになり意味が無いので、`Document::validate` が `INVALID_ARGUMENT`（`adjustment cannot cast a shadow or stroke`）で拒否する。既定。残りは許す。（新規）

### v3 だけに載せる

新しい効果は v3 の整数段だけを持つ。`Context::stage` の v3 でない枝（`self.resources.exact` が偽）は `RenderError::Internal` を返す。文書は v3 しか開けない（`DocumentMeta::validate`）ので到達しない。`v3_gap` の `integer_effect` と JS の `isIntegerEffect` に新しい種類を足す。足し忘れると `v3-unsupported` でシーンごと拒否され、`tests/e2e/parity-tests.js` が `scene gap` で落ちる。

### キャンバス編集

`scale_effects` を、すべての長さを持つ引数へ広げる。拡大後の値が引数の上限を超える、または非一様な拡大で等方でなくなるものは `UNSUPPORTED`（現行と同じ検査、文面は引数ごと）。`canvas.rotate` と `canvas.flip` は、`prepare_canvas_edit` が作る写像 `map`（整数の直交行列）の線形部分で向きを持つ引数を写す。

| 効果 | 一様な resize（倍率 s） | 非一様な resize | rotate / flip |
|---|---|---|---|
| `unsharp_mask`、`high_pass` | `radius` × s | `UNSUPPORTED` | 変化なし（新規） |
| `motion_blur` | `distance` × s | `UNSUPPORTED`（角度が変わる） | `angle` を、方向ベクトルを `map` で写して求め直す（90 度の倍数の加減と符号反転）（新規） |
| `noise` | 変化なし（粒は文書画素の格子に固定） | 変化なし | 変化なし |
| `drop_shadow` | `offset_x`、`offset_y`、`blur`、`spread` × s | `UNSUPPORTED`（`blur`、`spread` が等方） | `offset` の2成分を `map` で写す（新規） |
| `stroke_effect` | `width` × s | `UNSUPPORTED` | 変化なし（新規） |
| `gradient_overlay` | 変化なし（ノード座標） | 変化なし | 変化なし（新規） |
| `displacement_map` | `scale_x`、`scale_y` × s | `scale_x` × sx、`scale_y` × sy | `scale_x`、`scale_y` を `map` で写す（入れ替えと符号）（新規） |

`prepare_canvas_edit` は調整レイヤーの `adjustment` にも同じ更新をかける（現行の `Op::SetAdjustment` の経路）。

### 共通の文書形式

全 variant に既存と同じ `selection` と `extra` を持たせる。色は `[r, g, b, a]` の straight、各 0〜1。数値はすべて有限でなければならない。

| type | フィールド | 範囲 | 既定 |
|---|---|---|---|
| `unsharp_mask` | `radius`、`amount`、`threshold` | 0〜64、0〜5、0〜255 の整数 | 2、1、0（新規） |
| `motion_blur` | `angle`、`distance` | −180〜180 度、0〜128 文書 px | 0、8（新規） |
| `noise` | `amount`、`seed`、`monochrome` | 0〜1、u32、bool | 0.1、0、true |
| `high_pass` | `radius` | 0.5〜64 | 4（新規） |
| `drop_shadow` | `offset_x`、`offset_y`、`blur`、`spread`、`color`、`opacity` | ±4096、±4096、0〜64、0〜64、色、0〜1 | 4、4、8、0、黒、0.75（新規） |
| `stroke_effect` | `width`、`position`、`color`、`opacity` | 0〜64、`outside` / `inside` / `center`、色、0〜1 | 2、`outside`、黒、1（新規） |
| `gradient_overlay` | `gradient`、`opacity`、`blend` | FX-9、0〜1、`BlendMode` | 必須、1、`normal`（新規） |
| `displacement_map` | `map`、`scale_x`、`scale_y`、`wrap` | FX-8、±128、±128、`repeat` / `clamp` | 必須、10、10、`clamp`（新規） |

`offset_x`、`offset_y` は文書画素に丸める（`alto_core::q_from_f64_bits(bits, 0)` の half-away）。半径・距離・幅は f64 のビット列から Q8 を直接求める（`blur_radius_q8_f64` と同じ流儀、上限 16384）。色と不透明度は `as f32` のあと既存の `q14_round` 系で Q14 にする。（新規）

## FX-4 ハイパス

### 現状

- ぼかしの段は `Context::stage` の `REffectKind::GaussianBlur` の枝（水平・垂直の2 dispatch、モード 24）。`effects-q14.wgsl` の `blur_stage` と `blur_weight` が重みを読む。重みは `blur_kernel_q16`（`crates/alto-gpu/src/renderer.rs`）。
- 粗いレベルでの評価は `Context::stack` のぼかしの枝にだけある。結合に使える Q14 の straight 変換は `straight_q`（`blend-q14.wgsl`）。
- 独立参照：`blur_pass_reference`（Rust）、`blurPassQ14`（JS）。結合の参照は無い。

### 11-04 からの変化

- 11-04 は `m5.rs` の `input_roi_v3` と書いていた。`m5.rs` は 4121d69 で機能名へ改名され、725d6f4 で `crates/alto-gpu/src/renderer/tile_renderer/` の `stages.rs`、`graph.rs`、`params.rs` などへ分割された。`blur_stage` は `effects-q14.wgsl` に残る。
- `Effect` は 126f2b5 で `lib.rs` から `crates/alto-doc/src/effect.rs` へ移った。
- 11-04 は「ブラー段の出力を B に、元を A にして新モード 34 で結合」と書いていたが、`stack` の粗いレベルの扱い（拡大表示でぼかしを粗い格子で作る）と、選択つきぼかしの二重評価に触れていなかった。

### 設計

**式。** 結合の入力は A = 元（`desired` のレベル）、B = ぼかした平面（`crop` 後、`desired` のレベル）。Q14 の straight 値 `x = straight_q(A)`、`b = straight_q(B)` に対して `out_rgb = clamp(x − b + 8192, 0, 16384)`。アルファは `A.a` のまま。出力は premultiplied（`mul_q(out_rgb, A.a)`）。`A.a == 0` は 0 のまま。平坦な単色では RGB が 8192 になる。ぼかしは premultiplied の平面を通すので、アルファの縁では `b` が元の色と異なる。これが定義である。

**データ。**

- 文書：`Effect::HighPass { radius: f64, selection, extra }`。検証：`radius` は 0.5〜64（0 は恒等ではなく灰色になるので、下限を 0.5 にする）。
- 描画用：`REffectKind::HighPass { radius: f32, radius_q8: u32 }`（新規）。`radius_q8` は `blur_radius_q8_f64` と同じ。
- uniform：`settings` を空にし、ぼかしと同じ半径を `smoothing_radius_q8` に返す。結合モードは `settings` を使わない。（新規）

**ROI。** ぼかしと同じ（`effect_growth_q8` は 0、ぼかしの平面の広がりは `smoothing_radius_q8` で `stack` が入力 ROI に足す）。出力は入力と同じ範囲なので、`effect_output_bounds` は広げない。（新規）

**共通基盤（FX-4 で作る）。**

1. `smoothing_radius_q8`、`effect_growth_q8`、`RoiQ::grow` を作り、`stack` のぼかしの枝を一般化する。既存の `GaussianBlur` の挙動は変えない。（新規）
2. 評価の重複を避ける。各 `stack` のルート呼び出しに専用の評価結果のメモ（新規）を持たせ、再帰の間だけ `(効果の接頭辞の長さ, ROI)` をキーに共有する。別のノードや調整レイヤー、子孫の `stack` 呼び出しには共有しない。同じ `Context` は複数の入力や変換を扱うので、メモを `Context` 全体で共有すると別の入力と衝突する。粗い平面と元画像では ROI が異なるため、このメモだけで評価回数が線形になるとは限らない。異なる半径・倍率と選択範囲を組み合わせて評価回数と GPU 予算を測り、必要なら ROI をまとめる評価計画を追加する。メモは同じ `Target` の `lease` を共有し、既存の GPU 予算に計上する。予算が逼迫したら使用中でないメモの lease を解放し必要時に再計算する。再計算の増加も測る。必要な作業が予算に収まらない場合は既存の `RenderError::Budget`（`INVALID_ARGUMENT`）を返し、編集済み文書は維持する。
3. 調整レイヤーの判定（`graph.rs`）を述語へ置き換える。

### 形式と通信

- `format_version`：**上げる**（8。00-ROADMAP の番号の表）。新しい `type` を持つ文書を古い実装は読めない。manifest の `document_format` は `crates/alto-store/src/lib.rs` が 5 を固定で書き、読み込みも 5 以外を `package format` で拒否するので、同じ番号へ上げる変更をこの作業に含める（FX-4 の作業手順の Modify に `crates/alto-store/src/lib.rs`）。FX-1〜FX-6、FX-9 の新しい type は回 8 にまとめる。FX-8 は段階4なので別の回（16）。番号を上げる基盤（`FORMAT_VERSION` など）は回 6 の PR（06）が先に入れるので、この PR は `SUPPORTED_FORMATS` に 8 を足す。（新規）
- `PROTOCOL`：上げない。`setEffects` のペイロードは `unknown[]` のまま。
- 描画の版：v3 のまま。既存の画素と既存の parity 行は変わらない（`REffectKind` の追加だけ）。
- シェーダーのモード番号：ハイパスの結合 1 つ（34）。

### 作業手順

**Files:**
- Create `crates/alto-gpu/src/cpu_composite/effect_reference.rs`（新規）。
- Modify `crates/alto-doc/src/effect.rs`、`crates/alto-engine/src/selection.rs`、`crates/alto-scene/src/lib.rs`、`crates/alto-gpu/src/renderer/geometry.rs`、`crates/alto-gpu/src/renderer/tile_renderer/stages.rs`、`crates/alto-gpu/src/renderer/tile_renderer/graph.rs`、`crates/alto-gpu/src/renderer/tile_renderer/mod.rs`、`crates/alto-gpu/shaders/effects-q14.wgsl`、`crates/alto-gpu/src/cpu_composite/mod.rs`、`crates/alto-store/src/lib.rs`、`tests/e2e/cpu-composite.js`、`tests/e2e/parity-tests.js`、`tests/parity/v3-browser.txt`（追記のみ）、`packages/alto-sdk/src/types.ts`、`packages/alto-sdk/api/alto-engine.api.md`。

**Interfaces:** Produces `Effect::HighPass { radius, selection, extra }`、`REffectKind::HighPass { radius, radius_q8 }`、`smoothing_radius_q8`、`effect_growth_q8`、`RoiQ::grow`、評価のメモ、ハイパスの結合モード。

- [ ] 形式の試験：`document_format` を上げた manifest を保存・再オープンでき、旧番号は `package format` で拒否される。
- [ ] 基盤の試験（先に赤）：`selected_blur_stack_evaluates_each_prefix_once`（十分な予算でメモを保持できる間、同一の接頭辞・ROI を重複評価しない）。選択つきぼかしを異なる半径・倍率で最大 32 個重ねた場合も評価回数と GPU 予算を記録し、指数的な増加を防ぐこと。予算逼迫による解放・再計算も画素一致を検証する。既存のぼかしの parity（`blur-v3-*`）が1バイトも変わらないこと。
- [ ] 試験：同じ ROI・効果数を持つ異なるレイヤーと調整レイヤーを重ね、メモ有効時も無効時と全画素が一致する。ネストしたグループと異なる変換も含める。
- [ ] 試験：メモが領域を保持する場合も、従来成功した文書が同じ GPU 予算で描画できることと、予算超過時に既存のエラーを返し文書を保つことを確認する。
- [ ] 試験（`effect.rs`）`validate_rejects_high_pass_radius_below_half`、`high_pass_defaults_parse_and_roundtrip`、`known_effect_defaults_parse_but_unknown_fields_are_rejected` の形に新しい形を足す。
- [ ] 試験（`effect_reference.rs`）`high_pass_of_flat_color_is_mid_gray`（RGB が 8192、アルファ維持）、`high_pass_keeps_alpha_and_transparent_pixels`、`high_pass_of_straight_ramp_has_known_values`（手計算した値）。
- [ ] 試験（`stages.rs`）`high_pass_input_roi_matches_blur`（同じ半径で `input_roi_v3` が同じ）。
- [ ] WGSL、JS を実装し、parity 症例 `effect-high-pass`（平坦、勾配 `colorAdjustGradient`、タイルの継ぎ目をまたぐ矩形）、`effect-high-pass-selected`（選択つき）を追加。
- [ ] 完了条件：`git diff tests/parity` が追記のみ。`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional` が成功。opencode レビュー後に PR。

### 依存

- 先に要る項目：なし（QA-1、QA-5 が先に入っていると形式変更をファザーが検査する）。
- 後に続く項目：FX-1（結合段とぼかしの枝を共用）、FX-2、FX-5、FX-6（`smoothing_radius_q8`、`effect_growth_q8` を使う）、FX-8（`RoiQ::grow`）。（新規）PERF-4（10）は段階2の効果（FX-1〜FX-6、FX-9）の後に入り、`stages.rs`、`graph.rs` の再帰を明示タスクに直す（00-ROADMAP の重なりの表）。

## FX-1 シャープ（アンシャープマスク）

### 現状

- FX-4 と同じ（ぼかしの段、`stack` の枝、`straight_q`）。シャープに相当する効果は無い。

### 11-04 からの変化

- FX-4 と同じ（パスの改名・分割、`Effect` の移動）。11-04 は結合のモードを 35 と書いていたが、番号は 35（シャープの結合）とし、ハイパスの結合を 34 にした（00-ROADMAP の表）。

### 設計

**式。** FX-4 と同じ A、B。`d = x − b`（straight の Q14）。`|d| ≤ threshold_q14` なら `out = x`、それ以外は `out = clamp(x + ((amount_q8 * d + 128) >> 8), 0, 16384)`。`>>` は算術シフト（床）で、負の `d` も同じ式。`amount_q8 = round(amount × 256)`（f64 のビット列から）、`threshold_q14 = (threshold * 16384 + 127) / 255`（整数）。アルファは `A.a` のまま。`amount_q8 * d` は最大 1280 × 16384 = 2.1e7 で i32 に収まる。

**データ。**

- 文書：`Effect::UnsharpMask { radius: f64, amount: f64, threshold: f64, selection, extra }`。`threshold` は 0〜255 の整数（小数部があれば `INVALID_ARGUMENT`）。`amount` は 0〜5。`radius` は 0〜64（0 は恒等）。
- 描画用：`REffectKind::UnsharpMask { radius: f32, radius_q8: u32, amount_q8: u32, threshold_q14: u32 }`（新規）。
- uniform の `settings`：`amount_q8`、`threshold_q14`。（新規）

**ROI、粗いレベル。** FX-4 と同じ。`radius == 0` のときは平面を作らず、恒等（`amount` が何でも `d = 0`）。

### 形式と通信

- `format_version`：FX-4 と同じ回 8。`PROTOCOL`：上げない。描画の版：v3 のまま。
- シェーダーのモード番号：シャープの結合 1 つ（35）。

### 作業手順

**Files:** FX-4 の Modify と同じ（`effect_reference.rs` は Modify）。

**Interfaces:** Produces `Effect::UnsharpMask { radius, amount, threshold, selection, extra }`、`REffectKind::UnsharpMask { radius, radius_q8, amount_q8, threshold_q14 }`、シャープの結合モード。Consumes FX-4 の基盤。

- [ ] 試験（`effect.rs`）`validate_rejects_unsharp_amount_above_5`、`validate_rejects_unsharp_threshold_with_fraction`、`validate_rejects_unsharp_threshold_above_255`。
- [ ] 試験（`effect_reference.rs`）`unsharp_zero_amount_is_identity`、`unsharp_threshold_keeps_small_differences`（`|d|` が閾値以下の画素は元のバイトのまま）、`unsharp_negative_difference_rounds_by_floor`（負の `d` の丸めが3実装で同じ）、`unsharp_clamps_at_both_ends`。
- [ ] parity 症例 `effect-unsharp`、`effect-unsharp-threshold`、`effect-unsharp-tile-seam`。
- [ ] 完了条件・検証コマンドは FX-4 と同じ。

### 依存

- 先に要る項目：FX-4。
- 後に続く項目：なし。

## FX-2 移動ぼかし

### 現状

- 方向つきの畳み込みは無い。ぼかしは軸ごとの Gaussian（水平→垂直）。色相の回転が角度から sin、cos を求める整数 CORDIC を持つ（`effects-q14.wgsl` の `hue_sincos_q14`、角度は Q16 度、結果は Q14）。Rust と JS の独立実装は無い。

### 11-04 からの変化

- 11-04 は「ホストが sin、cos を Q16 の表（1/256 度刻みの CORDIC）で求める」と書いた。いまは既存の `hue_sincos_q14` がシェーダー側で CORDIC を持つので、それを共用する（角度は Q16 度の整数を uniform に入れる）。ただし Rust と JS の参照が無いので、本項目で作る。
- ほかは FX-4 と同じ（パスの改名、基盤）。

### 設計

**定義。** 角度方向の1次元の箱平均。`distance` の半分 `h_q8 = round(distance_q8 / 2)`（`distance_q8` は f64 のビット列から Q8、0〜32768）。`smoothing_radius_q8` は `h_q8`。サンプル平面のレベルの格子で `(r, rho8) = RoiQ::taps(h_q8)`（`r` は片側のタップ数、`rho8` は格子画素の Q8 の半径、`r ≤ 64`）。`r == 0` は恒等。（新規）

- 間隔 `spacing_q8 = (rho8 + r / 2) / r`（Q8 画素、≤ 256）。
- 方向 `(c, s) = hue_sincos_q14(angle_q16)`（Q14）。`angle_q16 = q_from_f64_bits(angle.to_bits(), 16)`（±180 度は 11796480）。
- 刻み `step = ((c * spacing_q8 + 128) >> 8, (s * spacing_q8 + 128) >> 8)`（Q14 画素）。
- タップ `i = −r..=r` の整数オフセット `off_i = ((i * step.x + 8192) >> 14, (i * step.y + 8192) >> 14)`（算術シフト）。
- `out = (Σ_i sample_q14(p + off_i) + taps / 2) / taps`、`taps = 2r + 1`。premultiplied の Q14 を4チャンネルとも同じ式で平均する。範囲外の画素は 0。

**粗いレベル。** 拡大表示で `r` が 64 を超えないよう、`stack` の枝（FX-4 の基盤）が `smoothing_radius_q8 = h_q8` を `blur_coarsen_k` の規則に渡す。平面は粗いレベルで作り `crop` で戻す。レベル 0 以下だけを扱うので、`step` は格子画素の整数計算で閉じる。

**データ。**

- 文書：`Effect::MotionBlur { angle: f64, distance: f64, selection, extra }`。
- 描画用：`REffectKind::MotionBlur { angle_q16: i32, distance_q8: u32 }`（新規）。
- uniform の `settings`：`r`、`spacing_q8`、`angle_q16`。（新規）

**ROI。** `effect_growth_q8` が `ceil(h_q8 / 256) * 256` を4辺（角度によらない上界）。実際の広がりは `|off_i|` の最大で、これ以下である。（新規）

### 形式と通信

- `format_version`：FX-4 と同じ回 8。`PROTOCOL`：上げない。描画の版：v3 のまま。
- シェーダーのモード番号：移動ぼかし 1 つ（36）。`hue_sincos_q14` は `main` から届くので float 禁止の検査に通る（現にある）。

### 作業手順

**Files:** FX-4 の Modify と同じ。

**Interfaces:** Produces `Effect::MotionBlur { angle, distance, selection, extra }`、`REffectKind::MotionBlur { angle_q16, distance_q8 }`、移動ぼかしのモード、`hue_sincos_q14` の Rust 参照と JS 参照（`effect_reference.rs`、`cpu-composite.js`）。

- [ ] 試験 `hue_sincos_q14_reference_matches_the_shader_on_all_angles`（Q16 度の全域 ±11796480 を刻み 4096 で、Rust 参照と GPU が一致。JS 参照も同じ表）。
- [ ] 試験 `motion_blur_zero_distance_is_identity`、`motion_blur_of_flat_color_is_flat`、`motion_blur_axis_aligned_is_a_box_average`（角度 0 と 90 度が手計算の箱平均）、`motion_blur_roi_expands_by_half_distance`（`input_roi_v3`）。
- [ ] 試験（`effect.rs`）`validate_rejects_distance_above_128`、`validate_rejects_angle_outside_range`。
- [ ] parity 症例 `effect-motion-blur`（0、45、90、−135 度）、`effect-motion-blur-tile-seam`（タイル境界をまたぐ矩形）。
- [ ] 試験（`canvas.rs`）`canvas_rotate_turns_the_motion_blur_angle`、`canvas_resize_scales_the_distance`、`nonuniform_resize_refuses_motion_blur`。
- [ ] 完了条件・検証コマンドは FX-4 と同じ。

### 依存

- 先に要る項目：FX-4（基盤）。
- 後に続く項目：なし。

## FX-3 ノイズ

### 現状

- 乱数を使う段は無い。共通規則6は、乱数を文書の画素の計算に使うときは決定的なハッシュにする、と定める。

### 11-04 からの変化

- 変化なし（ノイズの設計は座標のハッシュで、改名・分割された経路には触れない）。経路のパスだけ「共通の設計」の表に従う。`git log b2036ce..bdf26a9` で `effects-q14.wgsl` と `crates/alto-scene` に変化は無い。

### 設計

**ハッシュ。** 32 ビットの PCG 出力関数（Rust は `u32` の `wrapping_*`、WGSL は `u32`、JS は `Math.imul` と `>>> 0`）。

```
pcg32(v):  state = v * 747796405 + 2891336453   // 32 ビットで巻き戻る
           word  = ((state >> ((state >> 28) + 4)) ^ state) * 277803737
           return (word >> 22) ^ word
```

**画素の値。** 座標は**文書画素**（キャンバス座標）で、タイルや表示レベルに依存しない。ノードの変形にも従わない（レイヤーを動かすと絵が粒の下をずれる。粒を絵に固定する案は、再標本が要るので採らない）。格子レベル `L ≤ 0` の ROI の原点 `(ox, oy)` と画素 `(ix, iy)` から `x_doc = floor_div(ox + ix, 2^(−L))`（`L = 0` は `ox + ix`）、`y_doc` も同じ。つまり拡大表示では、1文書画素の粒が拡大画素にまたがる（最近傍）。（新規）

- `h = pcg32(x_doc as u32 ^ pcg32(y_doc as u32 ^ seed))`。
- 単色：`n = noise(h)` を R、G、B に加算。カラー：チャンネル `c = 0, 1, 2` ごとに `n_c = noise(pcg32(h ^ (c + 1)))`。
- `noise(h) = floor_div((2 * (h >> 16) − 65535) * amount_q14 + 32768, 65536)`（`amount_q14 = round(amount × 16384)`、値域は ±`amount_q14`）。積は ±1.07e9 で i32 に収まる。（新規）
- straight の Q14 に加算して `[0, 16384]` に切り、アルファは変えない。出力は premultiplied（`mul_q(color, a)`）。`a == 0` の画素は 0 のまま。

v3 のレベル 1 以上は子の縮小で作る（`compose_from_children`）ので、縮小表示で粒は平均され、レベル 0 の粒が定義である。

**データ。**

- 文書：`Effect::Noise { amount: f64, seed: u32, monochrome: bool, selection, extra }`。`seed` は JSON の整数（0〜4294967295）。`monochrome` は bool（数値などは `INVALID_ARGUMENT`）。
- 描画用：`REffectKind::Noise { seed: u32, amount_q14: u32, monochrome: bool }`（新規）。
- uniform の `settings`：`seed`、`amount_q14`、`monochrome`、`shift`（`−L`）。ROI の原点 `(ox, oy)` は `grid[6]` 以降の語に置く（`Levels` の `out_white` と同じ置き方）。（新規）

**ROI。** 広がらない（`effect_growth_q8` は 0）。（新規）

### 形式と通信

- `format_version`：FX-4 と同じ回 8。`PROTOCOL`：上げない。描画の版：v3 のまま。
- シェーダーのモード番号：ノイズ 1 つ（37）。

### 作業手順

**Files:** FX-4 の Modify と同じ（`stages.rs` は `Context::stage` の枝の追加だけ）。

**Interfaces:** Produces `Effect::Noise { amount, seed, monochrome, selection, extra }`、`REffectKind::Noise { seed, amount_q14, monochrome }`、ノイズのモード、`pcg32`（Rust、WGSL、JS）。

- [ ] 試験 `pcg32_matches_known_vectors`（3実装で同じ既知ベクトル。入力 0、1、0xFFFFFFFF、1234567 の出力を Rust の試験で固定し、JS と WGSL の dispatch で一致を確かめる）。
- [ ] 試験 `noise_is_independent_of_tile_origin`（同じ文書画素が、異なるタイル分割・異なる ROI の原点で同じ値）。
- [ ] 試験 `noise_zero_amount_is_identity`、`noise_keeps_alpha`、`noise_monochrome_adds_the_same_value_to_all_channels`、`noise_at_zoom_in_repeats_one_document_pixel`（レベル −2 で 4×4 格子画素が同じ値）。
- [ ] 試験（`effect.rs`）`validate_rejects_noise_seed_above_u32`、`validate_rejects_non_bool_monochrome`。
- [ ] parity 症例 `effect-noise`、`effect-noise-color`、`effect-noise-tile-seam`。
- [ ] 完了条件・検証コマンドは FX-4 と同じ。

### 依存

- 先に要る項目：なし。
- 後に続く項目：なし。

## FX-5 ドロップシャドウと出力範囲

### 現状

- 出力が入力より広がる効果は無い。ROI の逆算は対称な `RoiQ::expand(x, y)` だけ。効果つきシーンの変更範囲は `Damage::All`。
- 05 の IMG-1 が出力範囲を要求している（`effect_output_bounds`）。（新規）

### 11-04 からの変化

- 11-04 は `effect_output_bounds` を「変更範囲と描画範囲の両方に使う」と書き、「タイルの可視判定が効果の外へ広がる」ことを試験の対象にしていた。いまは、効果つきシーンは `scene_damage` が `Damage::All` を返し、レンダラーにノード範囲による除外が無い。だから変更範囲の試験（旧設計の「影の範囲が変更範囲に入る」試験）と可視判定の試験は意味を失い、必要なのは「描画の入力 ROI が影の分だけ広がること」と「IMG-1 に出力範囲を渡すこと」になった。（新規）
- 距離場（JFA）を FX-6 の中に書いていたが、スプレッドが使うので、距離場の段は本項目で作り、FX-6 が使う。
- パスの改名・分割は FX-4 と同じ。

### 設計

**平面の作り方。** 入力のアルファから影を作り、入力の背面に合成する。

1. 平面 P0 = 入力のアルファ（`A.a` を R、G、B、A の全チャンネルに）。`spread == 0` ならそのまま。
2. `spread > 0`：距離場（下）で `cov = clamp(spread_q8 + 128 − d_q8, 0, 256) << 6`（Q14）を求め、`P1.a = max(A.a, cov)`。
3. `blur > 0`：既存のぼかしの段（モード 24）で P1 をぼかす（半径 `blur_q8`）。（新規）
4. 移動：平面の ROI の原点を `−(ox, oy)` 格子画素ずらす（`input_map` が整数の texel 対応を作る）。移動量は `round(offset × 2^(−g))`（`g` は平面のレベル。`g ≤ 0` では整数）。
5. 色つき平面（モード：色つき平面）：`α = mul_q(mul_q(P.a, opacity_q14), color.a_q14)`、出力 = `(color_q14.rgb * α / 16384, α)` を premultiplied で。
6. 入力を上に Normal で合成：モード 19（`layer_q14`、dst = 色つき平面、src = 入力、不透明度 16384、合成モード 0）。

平面は粗いレベル（`smoothing_radius_q8 = max(blur_q8, spread_q8)`、FX-4 の基盤）で作り、`crop` で `desired` へ戻し、合成は `desired` で行う。

**距離場（FX-5 で作り、FX-6 が使う）。** Jump Flood（JFA）で、整数の距離の二乗を比べる。

- 種：`seed(p) = alpha(p) ≥ 8192`（Q14 の半分）。`inside` 用は反転（`alpha < 8192`）。平面（`rgba16uint` の R、G）に種の座標 + 1 を入れ、0 は種なし。座標は ROI の画素。ROI は `u16` に収まる（テクスチャの最大辺以下）。
- 1歩：画素 `p` が、`p + k * (dx, dy)`（`dx, dy ∈ {−1, 0, 1}`）の9か所の種を候補に、`d² = |p − seed|²` が最小のものを選ぶ。同点は `(seed.y, seed.x)` の辞書順で小さい方。ROI 外は無視。
- 歩幅：`R = ceil(width_q8 / 256)`（スプレッドは `ceil(spread_q8 / 256)`）、`m = ceil(log2(R + 1))`、歩幅は `2^(m−1)` から 1 まで（`m` 回、`R ≤ 64` で最大 7 回）。段は交互に2枚の目標を使う（ping-pong）。
- 距離：`d2c = min(d², (R + 1)²)`、`d_q8 = floor_isqrt(d2c << 16)`（固定回数の二分法、`u32`。`(65)² << 16 = 2.8e8` で収まる）。誤差は JFA の既知の少数の画素に出るが、手順が固定なので3実装で同じ結果になる。

**出力範囲の伝播（本項目で新設）。** 共通の設計の `effect_growth_q8` と `effect_output_bounds`（`crates/alto-scene/src/lib.rs`、新規）を実装する。（新規）

- `DropShadow` の広がりは `g = blur_q8 + spread_q8` と移動量から計算する（共通の設計の式）。（新規）
- `effect_output_bounds`：効果を順に適用し、各辺へ広がり量を `ceil` して足す。`i32` を超えると `None`。呼び出し側（IMG-1）は `None` を `RESOURCE_LIMIT` にする。（新規）
- 親グループの範囲：グループの出力範囲 = 子の出力範囲の合併 → グループ自身の効果（`effect_output_bounds`）。再帰関数 `node_output_bounds`（新規、`crates/alto-scene/src/lib.rs`）として提供する。（新規）
- いま `effect_output_bounds` の呼び出し元は IMG-1 だけである。レンダラーは `input_roi_v3` の逆算（同じ `effect_growth_q8` から出る）で十分で、変更範囲は `Damage::All` のままである。（新規）

**データ。**

- 文書：`Effect::DropShadow { offset_x, offset_y, blur, spread, color, opacity, selection, extra }`（`color: [f64; 4]`）。検証は共通の文書形式の表。ノード当たり 4 個（`MAX_DROP_SHADOWS_PER_NODE`）。調整レイヤーでは拒否。（新規）
- 描画用：`REffectKind::DropShadow { offset: [i32; 2], blur_q8: u32, spread_q8: u32, color: [f32; 4], opacity: f32 }`（新規）。

### 形式と通信

- `format_version`：FX-4 と同じ回 8。`PROTOCOL`：上げない。描画の版：v3 のまま。
- シェーダーのモード番号：距離場の初期化（38）、1歩（39）、距離から被覆（40）、色つき平面（41）の4つ（FX-6 も使う）。

### 作業手順

**Files:**
- Modify FX-4 と同じに加え、`crates/alto-scene/src/damage.rs`（変更しない。試験で `Damage::All` のままを固定するだけ）、`crates/alto-doc/src/node.rs`、`crates/alto-doc/src/document.rs`、`crates/alto-engine/src/canvas.rs`。

**Interfaces:** Produces `effect_growth_q8`、`effect_output_bounds`、`node_output_bounds`、`RoiQ::grow`（FX-4）の方向つき利用、`Effect::DropShadow`、距離場の4モード、`MAX_DROP_SHADOWS_PER_NODE`。

- [ ] 試験（`alto-scene`）`output_bounds_grow_by_offset_blur_and_spread`（移動量の正負で片側だけ広がる）、`output_bounds_of_color_effects_are_unchanged`、`output_bounds_overflow_returns_none`、`group_bounds_include_child_shadow`。
- [ ] 試験（`stages.rs`）`shadow_input_roi_grows_toward_the_light_opposite_side`（`input_roi_v3` が移動量の符号に応じた側へ広がる）、`shadow_roi_and_output_bounds_come_from_one_growth`（同じ Q8 から両方が出る性質試験：不透明な矩形の描画結果の非透明な範囲が `effect_output_bounds` に含まれる）。
- [ ] 試験（`effect_reference.rs`）`jfa_distance_matches_brute_force_except_known_pixels`（総当たりの最近傍と、一致しない画素の座標の一覧を固定）、`jfa_tie_break_prefers_smaller_y_then_x`、`shadow_of_opaque_rect_has_known_values`。
- [ ] ブラウザー試験 `a shadow from an offscreen layer is drawn`：ノードが画面外、影が画面内。
- [ ] 試験 `editing_a_shadowed_layer_still_damages_everything`（`scene_damage` が `Damage::All` のままで、古い影が残らない）。
- [ ] 試験（`effect.rs`、`document.rs`）`validate_rejects_a_fifth_drop_shadow`、`adjustment_rejects_drop_shadow`、`validate_rejects_shadow_color_outside_unit_range`。
- [ ] 試験（`canvas.rs`）`canvas_rotate_turns_the_shadow_offset`、`canvas_resize_scales_the_shadow`、`nonuniform_resize_refuses_a_shadow`。
- [ ] parity 症例 `effect-drop-shadow`、`effect-drop-shadow-spread`、`effect-drop-shadow-tile-seam`、`effect-drop-shadow-large-offset`（移動 > 64 画素の別評価）、`effect-drop-shadow-selected`。
- [ ] 完了条件：`git diff tests/parity` が追記のみ。GPU 予算：shadow を 4 個重ねた文書で `Budget` が `INVALID_ARGUMENT` として明示エラーになり、状態が壊れないこと。検証コマンドは FX-4 と同じ。

### 依存

- 先に要る項目：FX-4（基盤）。
- 後に続く項目：FX-6（距離場と色つき平面を使う）、05 の IMG-1（`effect_output_bounds`。無い間は 05 が暫定のぼかし半径で進められる）。（新規）

## FX-6 境界線

### 現状

- アルファの縁から線を描く効果は無い。ベクターの線（`stroke_q8.rs`）はパスの輪郭の多角形で、画素の効果ではない。距離場の段は FX-5 が作る。

### 11-04 からの変化

- 距離場（JFA）の初期化・歩・被覆の段を本項目に書いていたが、FX-5 のスプレッドと共用するので FX-5 へ移した。本項目は被覆から色つき平面へ、合成へ、の部分を持つ。
- パスの改名・分割は FX-4 と同じ。

### 設計

**平面の作り方。** 距離場（FX-5）で被覆 `cov` を作り、色つき平面（FX-5 のモード）にする。

- `outside`：種 = `alpha ≥ 8192`。`cov = clamp(width_q8 + 128 − d_q8, 0, 256) << 6`。色つき平面 S を作り、モード 19（dst = S、src = 入力）で入力を上に合成。
- `inside`：種 = `alpha < 8192`（補集合）。`cov` に `A.a` を掛けた（`mul_q(cov, A.a)`）平面 S を作り、モード 27（`clip_layer_q14`、基底 = 入力、src = S、不透明度 16384、合成モード 0）で入力の上に、入力のアルファを保って重ねる。
- `center`：両方を `width / 2` で適用する。先に `outside`（半分の幅）を作ってから、その結果に `inside`（半分の幅）を重ねる。
- 色つき平面の `α = mul_q(mul_q(cov, opacity_q14), color.a_q14)`。

距離は文書画素の格子ではなく、平面のレベルの格子で測る（FX-4 の基盤の粗いレベルの規則。幅を半径として渡す）。`outside` の `smoothing_radius_q8` は `width_q8`、`center` は `width_q8 / 2`、`inside` は `width_q8`。`width == 0` は恒等。（新規）

**データ。**

- 文書：`Effect::StrokeEffect { width: f64, position: StrokePosition, color: [f64; 4], opacity: f64, selection, extra }`。`StrokePosition` は `outside` / `inside` / `center`（新規、`crates/alto-doc/src/effect.rs`）。ベクターの線の `Stroke`（`alto-vector`）とは別物なので、名前を `StrokeEffect` にしている。（新規）
- 描画用：`REffectKind::StrokeEffect { width_q8: u32, position: u8, color: [f32; 4], opacity: f32 }`（新規）。
- ROI：入力の ROI を `ceil(width_q8 / 256)`（`center` は半分、`inside` は距離の探索のために同じ幅）で広げる。出力範囲は `outside` と `center` が広がる（`effect_growth_q8`）。（新規）

**判定条件（測って形が変わる）。** Metal と SwiftShader で、width 64、4K（3840×2160）のレイヤーの JFA の段の時間と、`Budget` が出る最小の文書を測り、結果を記録する。1フレーム予算（`doc/verification/ENGINE-HARDENING-RESULTS.md` の B1 の p95）を超える、または `Budget` が通常の文書で出るときは、`outside` だけを先に出し、`inside`・`center` は後続にする（`position` の `inside`・`center` は `UNSUPPORTED`）。速度の約束は書かない。

### 形式と通信

- `format_version`：FX-4 と同じ回 8。`PROTOCOL`：上げない。描画の版：v3 のまま。
- シェーダーのモード番号：新規なし（FX-5 の距離場の3つと色つき平面、既存の 19、27 を使う）。

### 作業手順

**Files:** FX-5 の Modify と同じ。

**Interfaces:** Consumes `effect_growth_q8`、距離場のモード、色つき平面のモード。Produces `Effect::StrokeEffect`、`StrokePosition`、`REffectKind::StrokeEffect`。

- [ ] 試験（`effect_reference.rs`）`stroke_outside_keeps_the_input_above`（入力の画素が不透明なら元のバイト）、`stroke_inside_stays_within_alpha`（入力のアルファが 0 の画素は 0）、`stroke_zero_width_is_identity`、`stroke_edge_coverage_falls_off_over_one_pixel`、`stroke_center_equals_outside_then_inside_at_half_width`。
- [ ] 試験（`effect.rs`）`validate_rejects_stroke_width_above_64`、`validate_rejects_unknown_stroke_position`。
- [ ] parity 症例 `effect-stroke-outside`、`effect-stroke-inside`、`effect-stroke-center`、`effect-stroke-tile-seam`。
- [ ] Metal で width 64 の段時間を測り、判定条件の結果を `doc/verification/` に記録（表の形は B1〜B8 の流儀）。
- [ ] 試験（`canvas.rs`）`canvas_resize_scales_the_stroke_width`、`nonuniform_resize_refuses_a_stroke`。
- [ ] 完了条件・検証コマンドは FX-4 と同じ。

### 依存

- 先に要る項目：FX-5（距離場、色つき平面、`effect_growth_q8`）。（新規）
- 後に続く項目：なし。

## FX-7 自動補正

### 現状

- `Levels` は `in_black`、`in_white`、`gamma`、`out_black`、`out_white`（0〜1、`in_black < in_white`）。ヒストグラムは `doc.image.histogram(node, range?)`（`packages/alto-sdk/src/document.ts`）→ `Engine::image_histogram`（`crates/alto-engine/src/canvas.rs`）。ラスターだけが対象（`raster required`）で、RGBA の各チャンネルを 256 個の度数にする。タイルの無い範囲は 0 として数える（透明な画素は R、G、B、A すべての 0 番目に入る）。ヒストグラムは効果・マスクの前の保存された画素である。

### 11-04 からの変化

- 変化なし（`image_histogram` と `Levels` はそのまま）。`packages/alto-sdk/src/document.ts` は版を外に持つ F-9 で変わったが（保存まわり）、`effects` と `image` の部分は同じ。

### 設計

**効果の種類は増やさない。** `doc.image.histogram` を使う SDK の補助で、`Levels` の引数を計算する。適用はしない（決定的で、文書に自動値は残らない）。

```ts
export type LevelsParams = Required<Pick<Extract<Effect, { type: "levels" }>, "in_black" | "in_white" | "gamma" | "out_black" | "out_white">>;
doc.effects.autoLevels(layer: NodeId, options?: { clip?: number }): Promise<LevelsParams>
```

- `clip`：0〜0.1（既定 0.001）。範囲外は `INVALID_ARGUMENT` 相当の `AltoError`（SDK の既存の型、`packages/alto-sdk/src/errors.ts`）。
- 手順：ヒストグラムを `histogram(layer)` で取る。`image_histogram` は保存された RGBA のバイトを、アルファを見ずに数える（タイルの無い画素だけが 0 番目に入る）。透明な画素を除くため、R、G、B の 0 番目から `a[0]`（アルファ 0 の画素の数）を引く。保存された透明画素の RGB が 0 でなければ除けず、その分は残る（既知の限界。厳密にするには `Engine::image_histogram` に「アルファ 0 を除く」引数を足す。既定は足さない）。下限は 0 で止める。チャンネル `c ∈ {r, g, b}` の総数 `N_c = Σ bin_c`。チャンネルごとの下端は、累積が `clip * N_c` を**超える**最小の値。上端は、上からの累積が `clip * N_c` を超える最大の値。`low` は3チャンネルの下端の最小、`high` は上端の最大（色相を保つため3チャンネルで共通の1組にする）。
- 結果：`high > low` なら `{ in_black: low / 255, in_white: high / 255, gamma: 1, out_black: 0, out_white: 1 }`。`high <= low`（平坦な画像、全透明）は恒等の `{ 0, 1, 1, 0, 1 }`（`in_black < in_white` を保つ）。
- 対象は `Raster` だけ。ほかは `Engine::image_histogram` の `INVALID_ARGUMENT` がそのまま返る。
- `Levels` はノードの効果として straight の値に掛かる。補助の入力は保存された画素（効果・マスクの前）なので、既にスタックに効果があるレイヤーでは、その効果の後の見た目に対する自動補正にならない。これを `doc.effects.autoLevels` の説明に書く。（新規）

### 形式と通信

- `format_version`：上げない（文書の型は増えない）。`PROTOCOL`：上げない（既存の `imageHistogram` を使う）。描画の版：変わらない。
- 公開 API が増えるので、`packages/alto-sdk/api/alto-engine.api.md` とブラウザー側の API レポートを更新する。

### 作業手順

**Files:** Modify `packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/types.ts`（`LevelsParams`）、`packages/alto-sdk/api/alto-engine.api.md`。Create `packages/alto-sdk/test/auto-levels.test.js`（新規）。

**Interfaces:** Produces `doc.effects.autoLevels(layer, options?)`、`LevelsParams`。

- [ ] Node 試験 `autoLevels clips the histogram ends`（片端に 0.1% 未満の外れ値がある画像で、外れ値を切った `in_black`、`in_white` が返る）、`autoLevels on a flat image returns the identity range`（`in_black < in_white` を保つ）、`autoLevels ignores transparent pixels whose stored RGB is zero`、`autoLevels rejects a clip outside 0 to 0.1`、`autoLevels does not change the document`（呼び出し前後で `getSnapshot` が同じ）。
- [ ] 検証：`pnpm --dir packages/alto-sdk build`、`node --test packages/alto-sdk/test/*.test.js`。ブラウザー側は `node scripts/verify-browser.mjs --functional` で `autoLevels` の結果を `doc.effects.set` に渡して描ける症例を1つ足す。
- [ ] 完了条件：API レポートの更新が差分に含まれる。opencode レビュー後に PR。

### 依存

- 先に要る項目：なし。
- 後に続く項目：なし。

## FX-8 複数入力の効果

### 現状

- 効果は `Vec<Effect>` の直列で、1つの入力しか読まない。効果の選択範囲（`selection`）はタイルの R8 を持ち、`validate_mask_plane_impl`（`crates/alto-engine/src/lib.rs`）が存在を検査し、`effect_hashes` が GC の根にし、描画は `mask_entries` + `lookup_entries`（`params.rs`）でアトラスから読む。
- 画像のアセットは `AssetKind::EncodedImage { mime }`（符号化されたバイトだけ。デコード後の画素は取り込み時にタイルへ入る）。`Op::SetAsset` で出し入れする。

### 11-04 からの変化

- 11-04 は `map: AssetId`（`EncodedImage` を取り込み時に専用タイルへ）と書いていた。`EncodedImage` のアセットはデコード前のバイトしか持たず、デコードは SDK 側（ブラウザー）で行うので、アセット ID だけではエンジンが画素を持てない。取り込みの経路は `importRgba`（`crates/alto-wasm/src/lib.rs`）が RGBA のタイルとアセット記録を同時に返す形だった。
- 11-04 は「出力範囲は入力と同じ」と書いたが、画素を変位で引く効果は、変位の分だけ内容が入力の範囲の外へ出る。出力範囲は軸ごとに `ceil(|scale|)` 広がる。
- `wrap` は11-04では入力の端の扱いのように読めたが、レイヤーは範囲の無い疎な平面なので、`wrap` はマップのほうの端の扱いとして定義し直した。

### 設計

**グラフにしない。** 設計書 3.7 の `EffectGraph { nodes, edges }` は、別のレイヤーを参照する効果が依存関係の変更範囲、ROI、GC の根を全部変えるので採らない。置き換えマップだけを、効果が持つ画像として入れる（ノード参照なし）。別の用途が出てから拡張する。

**マップの持ち方（既定：効果が RGBA8 のタイルを直接持つ）。**

```rust
pub struct DisplacementMap { pub bounds: ImageBounds, pub tiles: TileMap }   // 新規。tiles は Rgba8
```

選択範囲（`selection: Option<TileMap>`）と同じく効果の中にタイルのハッシュを持たせる。これにより、GC の根（`effect_hashes`）、保存・ジャーナル・版・差分・GPU 喪失後の再構築（アトラスへの再アップロード）が選択範囲の経路でそのまま動く。アセット ID にする案（11-04）は、デコード後の画素の置き場、使用中のアセットの削除の拒否（`Document::validate` に検査）、取り込みの経路をすべて新設する必要がある。利用者の決定が要る点ではないが、アセット ID に変えるなら本項目の作業が増える。

- 上限：マップの辺 ≤ 4096、タイル数 ≤ `MAX_DISPLACEMENT_MAP_TILES = 256`（新規）。超えると `INVALID_ARGUMENT`。`bounds` の外のタイルは拒否。`bounds` 内でタイルの無い部分は中立（R = G = 128）として読む。
- 検証：`validate_map_plane`（新規、`crates/alto-engine/src/lib.rs`。`validate_mask_plane_impl` は R8 専用なので Rgba8 版を別に書く）が、全タイルのハッシュが存在することを `Engine::transact` の前に確かめる。欠けていれば原子的に拒否し、文書は変わらない。（新規）
- 置き場所：マップはノードの座標に置く（選択範囲の `linked` と同じく、ノードの世界行列に従う）。変位量は文書画素。

**式。** 出力画素 `p`（格子レベル `L ≤ 0`）に対して、マップの画素 `m(p)` を `lookup`（最近傍、Q16 の逆行列。選択範囲と同じ）で読み、`(R, G)` のバイトを使う（A は無視）。

- `dx_grid = div_round_half_away((R − 128) * scale_x_q8 << (−L), 128 * 256)`。`dy_grid` も同じ（`G`、`scale_y_q8`）。`scale_*_q8` は f64 のビット列から Q8（±32768）。`(R − 128)` は ±128、積は ±2.7e8 で i32 に収まる。（新規）
- `out(p) = in(p − (dx_grid, dy_grid))`（正の値で内容が右・下へ動く）。読みは最近傍で、範囲外は 0。
- `wrap`：マップが `bounds` より外の画素を読むとき。`clamp` は `bounds` の端の画素、`repeat` は `bounds` の大きさで繰り返す（正の剰余）。
- ROI：入力の ROI を軸ごとに `ceil(|scale| * 2^(−L))` 広げる（`RoiQ::grow`）。粗いレベルは使わない（ぼかしではない）。

**データ。**

- 文書：`Effect::DisplacementMap { map: DisplacementMap, scale_x, scale_y, wrap: MapWrap, selection, extra }`。`MapWrap` は `repeat` / `clamp`（新規）。
- 描画用：`REffectKind::DisplacementMap { scale_x_q8: i32, scale_y_q8: i32, wrap: u8, map: RMask }`（新規。`RMask` は `tiles` と変換を持つ既存の型。`bounds` を足す）。

### 形式と通信

- `format_version`：**上げる**（16。段階4なので FX-4 の回 8 とは分ける。00-ROADMAP の番号の表）。`DisplacementMap` が RGBA8 のタイルのハッシュを持つので、保存・版・GC の経路が「効果が持つタイル」を知っている必要がある。GIT-1（08、段階3）が先に入り、`node_hashes` と `effect_hashes` を `Document::object_hashes`（新規、alto-doc）へ移す。本項目はその関数と `crates/alto-engine/src/lib.rs` の全ハッシュの列挙（`Op::SetProps` の経路）を新しい型へ広げる。GIT-1 より先に入る場合は `effect_hashes` を広げ、GIT-1 が移す。（新規）
- `PROTOCOL`：マップを SDK から渡す経路のために、`setEffects` の `effects: unknown[]` のままでよいか、RGBA を取り込む既存の `importRgba` の結果（`tiles`）を `Effect` の `map` として渡せるかを確かめる。渡せるなら上げない。新しい要求が要るなら上げる（14。段階4の回）。
- 描画の版：v3 のまま。
- シェーダーのモード番号：置き換え 1 つ（43）。

### 作業手順

**Files:** FX-4 の Modify に加え、`crates/alto-engine/src/lib.rs`（`validate_map_plane`、`effect_hashes` の経路）、`crates/alto-gpu/src/renderer/tile_renderer/params.rs`（`mask_entries` は R8 のマスク経路。Rgba8 のアトラス読み（`rgba_pixels` は Rgba8 をそのまま渡す）を使う別の入口を足す。GPU とストレージの経路の追加で、`RMask` に `bounds` を足すだけでは済まない）、`packages/alto-sdk/src/transaction.ts`（`ImageAsset` のタイルを効果へ渡す補助）。

**Interfaces:** Produces `Effect::DisplacementMap { map, scale_x, scale_y, wrap, selection, extra }`、`DisplacementMap`、`MapWrap`、`REffectKind::DisplacementMap`、置き換えのモード、`validate_map_plane`、`MAX_DISPLACEMENT_MAP_TILES`。

- [ ] 試験 `neutral_map_is_identity`（全画素 128 で入力と全バイト一致）、`displacement_moves_content_by_the_known_offset`、`displacement_reads_nearest_and_zero_outside`。
- [ ] 試験 `missing_map_tile_is_refused_atomically`（タイルが無い `Effect` の `SetProps` は文書を変えない）、`map_tiles_are_gc_roots`（GC のあとも残る）、`map_tile_count_above_limit_is_refused`。
- [ ] 試験 `wrap_repeat_and_clamp_differ_outside_bounds`。
- [ ] 試験（`stages.rs`）`displacement_input_roi_expands_by_scale`。試験 `displacement_output_bounds_grow_by_scale`（`effect_output_bounds`）。
- [ ] 試験 `map_is_rebuilt_after_gpu_loss`（GPU 喪失後にマップがタイルから再構築される。`tests/e2e/crash-tests.js` の `gpu-stroke` の流儀）。
- [ ] 保存と版の試験：`displacement_map_survives_save_open_and_version_restore`、`diff_reports_a_changed_map_as_effects`。
- [ ] parity 症例 `effect-displacement`、`effect-displacement-tile-seam`、`effect-displacement-repeat`。
- [ ] 試験（`canvas.rs`）`canvas_resize_scales_displacement_per_axis`、`canvas_rotate_swaps_displacement_axes`。
- [ ] 完了条件・検証コマンドは FX-4 と同じ。

### 依存

- 先に要る項目：FX-4（`RoiQ::grow`）、FX-5（`effect_growth_q8`、`effect_output_bounds`）。形式の番号は 16（00-ROADMAP の番号の表）。GIT-1（08。`Document::object_hashes`（新規））と STO-9（09。`tile_maps_mut`）が先に入り、本項目は両方に `DisplacementMap` を足す。PERF-4（10）を実装するなら、その明示タスクの形で段を足す（00-ROADMAP の重なりの表）。（新規）
- 後に続く項目：なし。

## FX-9 非破壊グラデーション

### 現状

- 非破壊のグラデーションは無い。ラスターへ確定する勾配は別物：`GradientOptions`（`crates/alto-engine/src/image/mod.rs`。`start`、`end`、`stops`（`offset` と `color: [f64; 4]`）、`kind` が `linear` / `radial`、`opacity`、`selection`）。停止点は `offset` で整列したあと**同じ offset を拒否**する（`duplicate gradient offsets`）。評価は `crates/alto-engine/src/image/pixels.rs` の浮動小数。
- 重ねる式は既存：モード 27（`clip_layer_q14`）が、基底のアルファを保ち、17 の合成モードで色を混ぜる。効果の選択範囲は `mask_entries` が世界行列に従って読む（ノード座標）。

### 11-04 からの変化

- 11-04 は「共通シェーダー関数を WGSL の共通ファイルから使う」と書いていた。07 の VEC-1 は、勾配の型を `alto-core` に置き、色の評価を `gradient-q14.wgsl`（新規）の `gradient_q14` に置く設計に決めた。FX-9 はこれを使う。
- 11-04 は結合を専用のモード（37）にしていたが、既存のモード 27 が「基底のアルファを保つ・17 の合成モード」をすでに持つので、新規のモードは勾配の平面を作る 1 つだけになる。
- 11-04 は `at` と狭義単調増加を書いたが、07 は `offset` と非減少（硬い境目）にしている。下で食い違いの案を示す。

### 設計

**平面と合成。**

1. 勾配の平面（新規モード）：画素ごとに `t_q16` を求め、`gradient_q14(t_q16, stops)` で straight の Q14 色（アルファ込み）を得て、premultiplied の平面に書く。入力は読まない。（新規）
2. 合成：既存のモード 27（`clip_layer_q14`、基底 = 入力、src = 勾配の平面、不透明度 = `opacity_q14`、合成モード = `blend`）。入力のアルファを保つので、ノードの透明な部分には勾配が出ない（Photoshop の「レイヤー効果：グラデーションオーバーレイ」と同じ）。`blend` は `BlendMode` の17種すべて。（新規）

**勾配の位置 `t`。** 座標はノード座標。評価は VEC-1 の `gradient_plan`（新規）と同じ手順（置き場は `crates/alto-gpu/src/raster_q8.rs`）：ホストがワールド行列（`FixedAffine`）とレベルから「平面の画素中心 → ノード座標」の逆写像を作り、`from`、`to`、`transform` と合成して整数係数にする（f64 の四則だけ、`q_from_f64_bits` で量子化）。シェーダーは係数と画素座標だけから `t_q16` を求める。線形は1次式、放射は2次式の `isqrt`（VEC-1 は固定回数の二分法）。ノード（またはレイヤー）の変形と親の変形に従う。

**型の食い違いと案（決定は ROADMAP で行う）。** 07 の `alto_core::Gradient`（新規）は FX-9 と共有する想定で、次が既存と異なる。

| 点 | ラスター確定（`GradientOptions`） | 07 の `Gradient` | 11-04 の FX-9 |
|---|---|---|---|
| 端点 | `start`、`end` | `from`、`to` | `from`、`to` |
| 停止点 | `offset`、`color: [f64; 4]` | `offset`、`color: [f32; 4]` | `at`、`color` |
| offset | 整列後に重複を拒否 | **非減少**（重複で硬い境目） | 狭義単調増加 |
| `transform` | 無し | あり（既定は単位） | 無し |

- 案 A（既定）：FX-9 は 07 の `Gradient` をそのまま使う（フィールド名は `offset`）。`gradient_overlay` の `gradient` は `Gradient`。硬い境目を許し、`transform` は単位のままでよい（SVG の gradientTransform に相当する用途が無い間は使われない）。本書の旧試験名（狭義増加を確かめるもの）は、非減少を確かめる名前に変える。ラスター確定の `GradientOptions` は直列化の形が違うので別のまま（共有しない）。（新規）
- 案 B：FX-9 だけ狭義増加にする。検証が2つになり、`Gradient::validate` を共有できない。非推奨。
- 案 C：07 を狭義増加にそろえ、硬い境目を禁止する。SVG の `<stop>` の同じ offset が取り込めなくなる。非推奨。
- どの案でも、ラスター確定の勾配（重複拒否）は変えない（既存の画素と文書を変えない）。

**色の補間：** 07 と同じ。停止点の色を `color_q14` で straight の Q14、`offset` を Q16 にし、`t` が属する区間（`offset[i] ≤ t` の最大の `i`。同じ offset が並ぶときは後ろ）で線形補間。端の外は端の色（pad）。補間は straight のまま。

**停止点の格納。** 16 停止点 × 3 語（`offset` 1 語、色 RGBA を 16 ビット × 4 で 2 語）= 48 語で、`Config.grid` の自由な語（`grid[6]` 以降の 40 語）に収まらない。案：(a) 効果では停止点を 8 個までにする（24 語）、(b) `tiles` の storage buffer（勾配のモードでは未使用）へ入れる、(c) `Config.unused` の 24 語を使う（`v3_stages_call_no_f32_coordinate_helper` の変更が要る）。既定は (b)。VEC-1 の勾配の記録と同じ形にする（先に着手した側が `GradientRecord`（新規）を決める）。

**データ。**

- 文書：`Effect::GradientOverlay { gradient: Gradient, opacity: f64, blend: BlendMode, selection, extra }`。検証は `Gradient::validate`（07。停止点 2〜16、`offset` 0〜1、端点の距離 ≥ 1/256、座標 ±1048576）に加え、`opacity` 0〜1。
- 描画用：`REffectKind::GradientOverlay { gradient: Gradient, opacity: f32, blend: RenderBlend }`（新規）。`alto-scene` は `alto-core` に依存済みなので `Gradient` をそのまま持てる。
- 広がりは無い（`effect_growth_q8` は 0）。（新規）

### 形式と通信

- `format_version`：FX-4 と同じ回 8。`Gradient` は VEC-1（07、回 7）が先に作るので、VEC-1 の回とは分ける（00-ROADMAP の重なりの表）。
- `PROTOCOL`：上げない。描画の版：v3 のまま。
- シェーダーのモード番号：勾配の平面 1 つ（42）。`gradient_q14` は `crates/alto-gpu/shaders/gradient-q14.wgsl`（新規、VEC-1 と共有）に置き、`integer_effects_shader` の連結に足す。（新規）

### 作業手順

**Files:** FX-4 の Modify に加え、Create `crates/alto-core/src/gradient.rs`（新規。VEC-1 が先なら Modify）、`crates/alto-gpu/shaders/gradient-q14.wgsl`（新規。VEC-1 と共有）。Modify `crates/alto-core/src/lib.rs`、`crates/alto-gpu/src/raster_q8.rs`（`gradient_plan`、VEC-1 が先なら不要）。（新規）

**Interfaces:** Produces `Effect::GradientOverlay { gradient, opacity, blend, selection, extra }`、`REffectKind::GradientOverlay`、勾配の平面のモード、共通 WGSL 関数 `gradient_q14(t_q16, stops) -> vec4i`（VEC-1 が再利用）。Consumes 07 の `Gradient`、既存のモード 27。

- [ ] 試験（`effect.rs`）`stops_must_not_decrease`（案 A のとき。案が変われば合わせる）、`gradient_overlay_rejects_one_stop`、`gradient_overlay_rejects_coincident_endpoints`、`gradient_overlay_opacity_out_of_range`、`gradient_overlay_roundtrips_through_json`。
- [ ] 試験（`effect_reference.rs`）`gradient_two_equal_stops_is_flat`、`radial_center_equals_first_stop`、`gradient_pads_outside_the_range`、`gradient_hard_edge_takes_the_later_color`、`overlay_keeps_the_input_alpha`（入力が透明な画素は透明のまま）。
- [ ] 試験 `gradient_follows_the_node_transform`（回転・拡大したレイヤーで、勾配がレイヤーに従う）。
- [ ] parity 症例 `effect-gradient-linear`、`effect-gradient-radial`、`effect-gradient-multiply`（合成モード）、`effect-gradient-tile-seam`。
- [ ] 試験（`canvas.rs`）`gradient_overlay_survives_canvas_resize_unchanged`（ノード座標なので値は変わらない）。
- [ ] 完了条件・検証コマンドは FX-4 と同じ。

### 依存

- 先に要る項目：07 の VEC-1（`Gradient`、`gradient-q14.wgsl`、停止点の格納 `GradientRecord`。持ち主は VEC-1 で、VEC-1 → FX-9 の順に入れる（00-ROADMAP の重なりの表）。本項目は案 A のとおり VEC-1 の型を使う）、FX-4 の基盤（調整レイヤーの判定の述語は不要だが、`REffectKind` の追加経路を共用する）。（新規）
- 後に続く項目：なし。

## Review Focus

- 効果を持つレイヤーの Undo と再描画で、効果の広がった部分に古い絵が残らないこと（効果つきシーンは `Damage::All`。この性質が崩れる変更を入れない）。
- レイヤー変形（回転、拡大）を挟んだ効果の半径・距離・幅が、既存のぼかしと同じ文書座標の単位であること。移動量と変位量も文書画素で、レイヤーの回転に従わない。
- `selection` 付きで、効果の外側が元の画素のままであること。
- 32 個の上限でスタックしたときに GPU 予算超過が `INVALID_ARGUMENT` として明示エラーになり、状態が壊れないこと。選択つきの連なりで評価が指数的に増えないこと。
- `canvas.resize`、`canvas.rotate`、`canvas.flip` で新しい効果の引数が「共通の設計」の表どおりに変わり、上限を超える拡大が拒否されること。
- 新しい段が float を使わない（`v3_stages_call_no_f32_coordinate_helper`）こと。既存の parity 行が1行も変わらないこと。
