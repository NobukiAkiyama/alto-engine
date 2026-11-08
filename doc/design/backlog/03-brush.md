# ブラシ

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 丸ブラシ以外の先端と、傾き・カーブによる動的な変化を追加する。

**Architecture:** ストロークは `StrokeSampler` がダブ列を作り、GPU がスタンプし、終了時に読み戻した結果を正本にする。履歴は結果で持つため、GPU 間の差は文書へ混ざらない。ダブ生成の変更は描画3実装に影響しない。

## 現状

- `crates/alto-raster/src/brush.rs`：`BrushConfig { size, hardness, spacing, opacity, flow, pressure_size, pressure_opacity }`。`Dab { center, radius, opacity, angle }` の `angle` は常に 0。`StrokeSample.tilt` は受け取るが未使用。
- `crates/alto-gpu/shaders/brush.wgsl`：円の距離だけを見る。
- `crates/alto-gpu/src/stroke_q8.rs`：v3 のストロークのカバレッジ。
- `packages/alto-sdk/src/tools.ts`：`BrushOptions`。`tip: { size, hardness, spacing }`。
- `AssetKind` には `BrushTip` と `Pattern` が既にあるが、寸法を持たず、読み込み API と描画の経路がない。

## SDK の形

```ts
interface BrushOptions {
  tip?: {
    size?: number; hardness?: number; spacing?: number;
    angle?: number;      // BR-1。度、既定 0
    roundness?: number;  // BR-1。0.01〜1、既定 1
    image?: AssetId;     // BR-2
    texture?: { asset: AssetId; scale?: number; depth?: number }; // BR-3
  };
  dynamics?: {           // BR-4、BR-5
    size?: BrushCurve; opacity?: BrushCurve; flow?: BrushCurve;
    angleFromTilt?: boolean;
  };
}
type BrushCurve = { input: "pressure" | "speed" | "tilt"; points: [number, number][] };
```

`pressureSize`、`pressureOpacity` は未リリースのため `dynamics` に統合して削除する。

## BR-4 傾きの反映

**設計。** `angleFromTilt` のとき `angle = atan2(tilt_y, tilt_x)`、`tilt` の大きさを入力 `"tilt"` のカーブへ渡す。`tilt` は度（−90〜90）で、入力値は `min(hypot(tx, ty) / 90, 1)`。

## BR-5 筆圧カーブ、速度連動

**設計。** カーブは 2〜8 点の区分線形。`x` は狭義単調増加、両端 0 と 1 を含む。速度は `t_ms` の差と距離から `px/ms` を計算し、指数移動平均（係数 0.3）で平滑化し、`min(v / 4, 1)` へ正規化する。ダブ生成は決定的な純粋関数で、入力サンプルが同じなら同じダブ列になる。

## BR-1 先端の角度と丸み

**設計。** ダブの形を楕円にする。`Dab` に `roundness` を追加する。`brush.wgsl` で点をダブ中心へ移し、`-angle` 回転、短軸方向を `1 / roundness` 倍してから距離を取る。v3 のストローク（`stroke_q8.rs`）も同じ変形を整数で行う。ダブの影響タイルの計算は外接円のままとする（保守的で安全）。

## BR-2 画像の先端

**設計。** R8 の先端テクスチャをアセットとして持つ。

- 既存の `AssetKind::BrushTip` を `BrushTip { width, height }` へ変える。形式は R8。辺 16〜1024、白がカバレッジ。
- `engine.assets.importBrushTip(blob)`：画像を輝度へ変換して内容ハッシュで保存する。GC の根に含める。
- 描画：ダブごとに先端テクスチャをサンプルする。`brush.wgsl` は GPU のフィルタを使ってよい。結果は読み戻しで確定するので、ハードウェア差は履歴へ混ざらない。
- `Dab` の `size` は先端の長辺に対応させる。

**注意。** ストローク中のプレビューと確定結果が同じ経路であること。現在の v3 確定経路（`stroke_q8.rs`）が CPU と一致を要求する箇所があるので、実装前に整数の双線形で揃えるか、ストローク確定だけ一致を要求しない分類にするかを試験で確かめる。

## BR-3 テクスチャブラシ

**設計。** カバレッジに紙目のパターンを掛ける。パターンは文書座標で固定する（ストロークと動かない）。

`coverage' = coverage × (1 − depth × (1 − pattern(x_doc × scale, y_doc × scale)))`。

パターンは既存の `AssetKind::Pattern` を `Pattern { width, height }` へ変えた R8 のアセットで、タイル状に繰り返す。

## BR-6 Undo の深さの設定

**設計。** `alto-doc` の `max_undo` は 100 に固定で、外から変えられない。`createEngine({ undo: { steps?: number } })` で 1〜1000 を指定する。IMAGE-EDITING の実装順の残りである。タイルの総量の上限は既存の `undos_over_budget` の計算に従い、増やさない。

## 上限

ダブのバッチ上限は現状を維持する。先端テクスチャの辺は 1024 まで。カーブは 8 点まで。文書で使用中の先端の削除は原子的に拒否する。

## 実装タスク

### Task BR-4 / BR-5: 動的な変化

**Files:** Modify `crates/alto-raster/src/brush.rs`、`crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/tools.ts`、`packages/alto-sdk/api`、Playground の呼び出し。

**Interfaces:** Produces `BrushCurve { input: CurveInput, points: Vec<(f32, f32)> }`、`BrushDynamics { size, opacity, flow, angle_from_tilt }`、`BrushConfig.dynamics`。

- [ ] 試験 `curve_interpolates_and_clamps`、`curve_rejects_non_increasing_points`。
- [ ] 試験 `angle_from_tilt_sets_the_dab_angle`、`no_tilt_keeps_the_previous_angle`（`atan2(0, 0)` で角度が飛ばない）。
- [ ] 試験 `speed_curve_uses_smoothed_speed`、`repeated_timestamp_does_not_divide_by_zero`。
- [ ] 試験 `the_same_samples_make_the_same_dabs`。
- [ ] 実装、`sh scripts/check.sh native`、PR。

### Task BR-1: 楕円の先端

**Files:** Modify `crates/alto-raster/src/brush.rs`、`crates/alto-gpu/shaders/brush.wgsl`、`crates/alto-gpu/src/stroke_q8.rs`、`tests/e2e/browser-tests.js`。

**Interfaces:** `Dab.roundness: f32`。Dab のバッファレイアウトは `extra` の未使用成分へ入れる。

- [ ] 試験 `round_dab_with_roundness_one_is_unchanged`（既存のストロークの画素が不変）。
- [ ] ブラウザ試験 `an elliptical brush is wider along its angle`。
- [ ] 実装、PR。

### Task BR-2 / BR-3: 画像先端とテクスチャ

**Files:** Modify `crates/alto-doc/src/lib.rs`（`AssetKind`）、`crates/alto-gpu/shaders/brush.wgsl`、`crates/alto-gpu/src/renderer.rs`、`packages/alto-sdk/src/engine.ts`、GC の根（`crates/alto-engine/src/versions/gc.rs`）。

**Interfaces:** `engine.assets.importBrushTip(blob): Promise<AssetRef>`。

- [ ] 試験 `removing_a_tip_in_use_is_refused`（アセットがストロークに使われるのは確定後は残らないので、使用中の判定はストローク中のセッションのみ）。
- [ ] 試験 `brush_tip_survives_save_reopen_and_gc`。
- [ ] ブラウザ試験 `a stamped tip draws its shape`、`GPU loss during a textured stroke rolls back`。
- [ ] 実装、PR。

### Task BR-6: Undo の深さ

**Files:** Modify `crates/alto-doc/src/lib.rs`、`crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/engine.ts`、`packages/alto-sdk/api`。

- [ ] 試験 `undo_steps_limit_the_stack`、`undo_steps_out_of_range_is_invalid`、`lowering_the_limit_keeps_the_newest_steps`。
- [ ] 実装、PR。

## Review Focus

- `tilt` を持たないマウス入力で、値が 0 でも動作が変わらないこと。
- 既存の丸ブラシの結果（parity の行）が全く変わらないこと。
- 細い楕円（roundness 0.01）で距離の計算がゼロ除算にならないこと。
- 回転した表示やレイヤー変形の下で、角度が文書座標かレイヤー座標かを統一すること。レイヤー座標に統一する。
- GPU 喪失時に先端テクスチャが内容ハッシュから再構築されること。
