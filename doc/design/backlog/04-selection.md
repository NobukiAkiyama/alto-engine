# 選択

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 既存の R8 選択を生成・加工する手段を揃える。

**Architecture:** 選択は文書座標の R8 タイルで、`Op::SetSelection` が確定する。生成はすべて `crates/alto-engine/src/m5/mod.rs` の `kind` の分岐を通る CPU 処理で、描画の3実装に影響しない。

## 現状

- Rust：`rectangle`、`ellipse`、`lasso`、`all`、`invert`、`clear`、`color`、`contiguous`。`color` と `contiguous` は `generated_selection`（`layer`、`tolerance` 0〜255、`x`、`y`、`r`、`g`、`b` などを読む）。
- SDK：`SelectionSpec.kind` に `color` と `contiguous` がない。型の外なので TypeScript からは呼べない。
- 上限：`MAX_SELECTION_TILES = 4096`、`MAX_SELECTION_SOURCE_TILES = 1024`。

## SEL-1 色域・連続領域の SDK 公開

`SelectionSpec` を判別共用体にする。

```ts
| { kind: "color"; layer: NodeId; r: number; g: number; b: number; a?: number; tolerance?: number; mode?: SelectionMode }
| { kind: "contiguous"; layer: NodeId; x: number; y: number; tolerance?: number; mode?: SelectionMode }
```

実際に読むキーは `generated_selection` に合わせ、実装時に一致を確かめる。

## SEL-2 拡大、縮小、境界

```ts
{ kind: "modify"; operation: "grow" | "shrink" | "border" | "smooth"; radius: number }
```

半径は 1〜64 の整数。現在の選択が無いときは `INVALID_ARGUMENT`。

**アルゴリズム（整数、CPU）。** 半径 r の円形の構造要素を行ごとの半幅の表 `w[dy]` として作る。

- `grow`：R8 の灰色膨張（max）。ぼかしのある選択の柔らかさを保つ。
- `shrink`：灰色収縮（min）。選択の外（タイルが無い部分）は 0。
- `border`：`grow − shrink`（飽和減算）。
- `smooth`：半径 r の箱平均の後、128 で 2 値化しない（柔らかいまま）。

行ごとの最大値は van Herk 法で半径に依存しない計算量にする。出力の範囲は入力の範囲を r だけ広げ、`MAX_SELECTION_TILES` を超えると拒否する。

## SEL-3 パスからの選択

```ts
{ kind: "path"; path: PathData; fill_rule?: FillRule; feather?: number; mode?: SelectionMode }
```

`canvas.rs` の `VectorRasterizer` と同じ注入で `raster_q8` の整数ラスタライザを使い、R8 へ変換する。パスの座標は文書座標。ラスタライザの注入は `alto-engine` が GPU に依存しないための既存の形で、`alto-wasm` の `image_versions.rs` が `alto_gpu::raster_q8` を渡している。

## SEL-4 被写体選択

除外（AI 機能）。記録は [12-collab-ai.md](12-collab-ai.md)。

## 実装タスク

### Task SEL-1: SDK の型

**Files:** Modify `packages/alto-sdk/src/document.ts`、`packages/alto-sdk/api`、`tests/e2e/m5-tests.js`。

- [ ] Rust の `generated_selection` が読むキーを確認し、型を書く。
- [ ] ブラウザ試験 `a contiguous selection from the SDK stops at the color edge`、`a color selection selects both separate areas`。
- [ ] API check とブラウザ試験を実行し、PR。

### Task SEL-2: 選択の加工

**Files:** Modify `crates/alto-engine/src/m5/mod.rs`、`packages/alto-sdk/src/document.ts`。

**Interfaces:** Produces `fn modify_selection(coverage: &Coverage, op: SelectionOp, radius: u32) -> Result<Coverage, EngineError>`。

- [ ] 試験 `grow_then_shrink_of_a_rect_is_the_rect`（角が丸まるので、辺の中央で確かめる）。
- [ ] 試験 `border_is_empty_inside_and_outside`。
- [ ] 試験 `grow_crosses_tile_boundaries`、`shrink_to_nothing_clears_the_selection`。
- [ ] 試験 `modify_without_selection_is_invalid`、`grow_past_the_tile_limit_is_refused_atomically`。
- [ ] 試験 `modify_is_one_undo_step`。
- [ ] 実装、PR。

### Task SEL-3: パスからの選択

**Files:** Modify `crates/alto-engine/src/m5/mod.rs`、`crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/document.ts`。

**Interfaces:** Consumes `VectorRasterizer`。

- [ ] 試験 `path_selection_matches_rectangle_selection`（同じ長方形のパスで同じタイルハッシュ）。
- [ ] 試験 `even_odd_hole_is_not_selected`、`path_selection_respects_add_and_subtract`。
- [ ] 試験 `huge_path_is_refused_by_the_tile_limit`。
- [ ] 実装、PR。

## Review Focus

- キャンバスより外へ広がる選択の扱い。既存の生成と同じ範囲に揃える。
- ぼかしのある選択への `shrink` で、値が飛ばないこと。
- 選択なしと「空の選択」を区別する既存の規則（空はどこにも描けない）を、`shrink` の結果にも適用すること。
- 大半径の `smooth` が UI スレッドでなく Worker で実行されること。
- レイヤーの `color` 選択で、レイヤー変形を持つ場合の扱いを既存の `offset` の制限と揃えること。
