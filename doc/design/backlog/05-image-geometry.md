# 焼き込みと幾何

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 拒否している焼き込みの条件を減らし、表示用のミップと高度な変形を設計する。

**Architecture:** 確定編集は `crates/alto-engine/src/canvas.rs` と `image.rs` の CPU 処理。効果付きの焼き込みは CPU に効果の実装がない（`cpu_composite.rs` は `#[cfg(test)]` で本番に含まれない）ため、GPU の1:1合成を読み戻して正本にする。

## 現状

- `plain_props` がマスク、ベクターマスク、クリップパス、効果、下にクリップ、非表示、Normal 以外を拒否する。
- `prepare_rasterize`：ベクターは整数位置・等倍のみ。テキストは `only raster and vector layers can be baked`。
- `VectorRasterizer` の注入で `raster_q8::vector_pixels_q14` を使う。
- 書き出しは `worker.ts` の `exportImage` が GPU の表示タイルを読み戻す。

## IMG-1 属性付きの焼き込み

**設計。** 焼き込みを2段にする。

1. **対象の合成。** ノードを `Isolated` の一時シーンとして、文書画素 1:1 で GPU に描き、読み戻す。効果、マスク、クリップパスが効いた結果が得られる。
2. **確定。** 読み戻した画素を `Op::ReplaceNode` で Raster にし、変形を恒等（整数オフセット）、効果とマスクを空にする。合成モードと不透明度はノードに残す。

下にクリップされたレイヤー（`clip_to_below`）は基底のアルファに依存するので、結果に基底の範囲を焼く選択肢を持たせず、`UNSUPPORTED` を維持する。非表示も `a hidden layer would be lost` を維持する。

`mergeDown` は下レイヤーと上の合成結果の見た目が変わらない条件を確かめる。下の不透明度が1で Normal のときだけ、上の合成モードを画素へ畳み込める。それ以外は拒否する。

非同期のため、`prepare_layer_edit` の同期経路に乗らない。Worker の新要求 `layerBake { node }` が GPU の結果を受けてから、`prepare_layer_edit` と同じ形の準備結果を作って確定する。読み戻しの間に文書が変わったら `CONFLICT`。

**範囲。** 効果の出力範囲は FX-5 の `effect_output_bounds` を使う。上限は `MAX_IMAGE_PIXELS`（64M）。

## IMG-2 任意変形のベクターのラスタライズ

**設計。** 整数位置・等倍の制限を外す。レイヤー変形を `FixedAffine` のままラスタライザへ渡し、文書画素 1:1 の窓で評価する。`vector_pixels_q14` は既に `world` を受け取るので、変更は `canvas.rs` の検査と `vector_tiles` の範囲計算に閉じる。ノードの `transform` は整数オフセットへ置き換える。`FixedAffine` の範囲外は `UNSUPPORTED`。

## IMG-3 テキストのラスタライズ

**設計。** テキストは既に焼き込み（`TextBake`）とアウトラインを持つ。アウトラインを `StyledPath` にして IMG-2 の経路で確定する。24px 未満で表示経路が違うアトラスの見た目とは差があるが、確定結果は1文書画素で一意である。フォールバック枠（未表示文字）は描かず、結果に `missing` の文字数を返す。

## IMG-4 コンテンツミップマップ

**設計。** IMAGE-EDITING の設計どおり、正本ハッシュとレベルを鍵とする派生ミップ。現状は表示タイルの階層（ピラミッド）があり、これが同じ問題の大半を解決している。ミップの目的は、大きな文書の低倍率で親を作るコスト削減である。

- 鍵：`(ContentHash, level)`。値は `rgba8unorm` の 256×256 タイル。文書へ書かない。
- 生成：2×2 平均（既存の `downsample-q14.wgsl` と同じ式）。
- v3 のレベル1以上の定義は「子の縮小」なので、ハッシュ固定を変えない条件は「ミップから作った親と、子から作った親がバイト一致」。これは透明と半透明の合成順序が違うと成立しない。**単一の不透明レイヤーに限って経路を選べる。** それ以外は従来経路にする。
- 判定条件：B2 の改善を測り、PERF-1 より効果が小さければ見送る。

## IMG-5 画像編集の GPU 高速化

**設計。** 既存の判断（読み戻しと符号化が残る）を維持し、測定が先。`crates/alto-engine/src/image.rs` のベンチで 4K の `resize`、`gradient`、`fill` の内訳を測り、符号化の比率が 50% 未満の操作だけを候補にする。正本は CPU の結果であるため、GPU 経路は3実装の parity を要する。

## IMG-6 遠近法、メッシュ変形、液化

**設計。** Affine2 の非破壊変形では表せないため、すべて確定編集とする（レイヤーの属性を増やさない）。

```ts
doc.image.warp(node, { kind: "perspective", corners: [[x, y], ...4] }, { filter: "bilinear" })
doc.image.warp(node, { kind: "mesh", cols, rows, points: [[x, y], ...] })
doc.image.warp(node, { kind: "displace", field: Int16Array /* Q4 の dx, dy */, width, height })
```

- 全て「出力画素→入力座標」の整数逆写像で、`resize_pixel` と同じ固定小数のフィルタを使う。
- 遠近は 3×3 行列を Q32 で。除算は i128。
- 液化は `displace` の変位場を SDK 側のブラシで作る。文書に変位場を保存しない。
- 上限：メッシュ 64×64、変位場は 4096²。範囲は `MAX_IMAGE_PIXELS`。

本機能は元の設計で対象外だったが、方針の変更で実装の対象とした（00 の D1）。

## 実装タスク

### Task IMG-3 / IMG-2: 任意変形のラスタライズ

**Files:** Modify `crates/alto-engine/src/canvas.rs`、`crates/alto-engine/src/text.rs`、`crates/alto-wasm/src/image_versions.rs`。

**Interfaces:** Consumes `VectorRasterizer`。Produces `fn text_paths(&self, node: &Node) -> Result<Vec<StyledPath>, EngineError>`。

- [ ] 試験 `rotated_vector_rasterizes_at_document_resolution`、`scaled_vector_matches_a_vector_drawn_at_that_size`。
- [ ] 試験 `text_rasterize_keeps_id_name_opacity_and_blend`、`text_rasterize_reports_missing_characters`。
- [ ] 試験 `rasterize_is_one_undo_and_survives_save`、`out_of_range_transform_is_refused`。
- [ ] 実装、PR。

### Task IMG-1: GPU を通した焼き込み

**Files:** Modify `crates/alto-wasm/src/image_versions.rs`、`crates/alto-gpu/src/renderer/m5.rs`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/document.ts`。

**Interfaces:** `doc.image.rasterize(node, { bakeProps: true })`。Consumes `effect_output_bounds`（FX-5）。Protocol を上げる。

- [ ] ブラウザ試験 `baking a blurred masked layer looks the same`：焼き込み前後の合成が全バイト一致。
- [ ] ブラウザ試験 `baking refuses a clipped layer`、`an edit during the bake readback is a conflict`。
- [ ] ブラウザ試験 `GPU loss during a bake leaves the document unchanged`。
- [ ] 試験 `merge_down_refuses_when_it_would_look_different`。
- [ ] 実装、PR。

### Task IMG-4: ミップの判定

**Files:** Create `doc/verification/IMG4-MIPMAP-MEASURE.md`。

- [ ] B2 と大きな単一画像の低倍率を測り、PERF-1 と比べて採否を記録する。

### Task IMG-5: GPU 高速化

**Files:** Create `doc/verification/IMG5-IMAGE-EDIT-MEASURE.md`。判定を通った操作について Create `crates/alto-gpu/shaders/image-edit-q14.wgsl`、Modify `crates/alto-wasm/src/image_versions.rs`。

- [ ] 4K の `resize`、`gradient`、`fill` の内訳を native と WASM で測り、判定条件で対象を決める。
- [ ] 対象の操作の試験 `gpu_edit_matches_the_cpu_bytes`（3つの GPU で全バイト一致）。一致しない入力は CPU 経路へ落とす。
- [ ] 試験 `GPU loss during an accelerated edit falls back to the CPU`。
- [ ] 実装、PR。

### Task IMG-6: warp

**Files:** Create `crates/alto-engine/src/warp.rs`。Modify `crates/alto-engine/src/image.rs`、`packages/alto-sdk/src/document.ts`。

- [ ] 試験 `identity_perspective_is_identity`、`identity_mesh_is_identity`、`zero_displacement_is_identity`。
- [ ] 試験 `singular_perspective_is_refused`、`warp_selection_limits_the_change`。
- [ ] 実装、PR。

## Review Focus

- 焼き込みの読み戻し中に Undo や別の編集が走る場合の順序。
- 効果がノードの範囲外へ広がるときに切れないこと。
- 非表示の親グループの中のレイヤーの焼き込み。親の不透明度は焼かないことを文書で明記する。
- 透明な範囲を持つ `ImageBounds` が焼き込み後に正しいこと。
- 8bit の量子化で、焼き込み後の見た目が Q14 の中間より1/255ずれること。焼き込みは RGBA8 に丸めるため、効果の連鎖の途中では一致しない。この差を試験の条件に明記する。
