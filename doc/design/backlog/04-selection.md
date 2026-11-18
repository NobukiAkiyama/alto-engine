# 選択

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 既存の R8 選択を生成・加工する手段を揃える。

**Architecture:** 選択は文書座標の R8 タイル（`Document.selection: Option<TileMap>`）で、`Op::SetSelection` が確定する。生成と加工はすべて `crates/alto-engine/src/selection.rs` の `Engine::selection_ops` が `spec["kind"]` で分岐する CPU 処理で、描画の3実装（CPU 参照、JS 参照、WGSL）に影響しない。SEL-3 だけは整数ラスタライザを注入で受け取る。Worker の中の WASM が実行するので、UI スレッドは止まらない。

## 共通の現状

- 入口：SDK `DocumentHandle.selection.apply(spec)`（`packages/alto-sdk/src/document.ts`）→ プロトコル `select`（`packages/alto-sdk/src/protocol.ts`、`spec: unknown`）→ `packages/alto-sdk/src/worker.ts` → `alto-wasm` の `select`（`crates/alto-wasm/src/lib.rs`）→ `Engine::selection_ops`（`crates/alto-engine/src/selection.rs`）→ `command("selection.set", ops)`。1回の `select` は1回の Undo に対応する。
- 選択の保持：`None` は選択なし（どこでも編集できる）。`Some` でタイルが空の選択は「空の選択」で、ブラシ・塗りなどの画像編集は `EMPTY_SELECTION` で拒否される（`crates/alto-engine/src/image/mod.rs`）。効果へ取り込む `mask_for_node` も空の選択は空のマスクにする。
- エラー：`selection.rs` の `invalid()` は `DocError::Invalid` で、コードは `INVALID_ARGUMENT`。上限超過も同じコード（`"Selection/mask exceeds 4096 tiles"`）。`canvas.rs` の拡縮は `RESOURCE_LIMIT` を使う。
- 上限（堅牢化 A〜F の後も同じ。`doc/MILESTONES.md`「上限・継続課題」2、3 と一致）：`MAX_SELECTION_TILES = 4096`、`MAX_SELECTION_SOURCE_TILES = 1024`（どちらも `crates/alto-engine/src/selection.rs`。`crates/alto-engine/src/canvas.rs` にも同値の private 定数 `MAX_SELECTION_TILES` がある）、ぼかし幅 0〜64、投げ縄 3〜8192 点。
- 既存の生成の範囲：長方形・楕円・投げ縄・全体・反転は文書の範囲 `[0, width) × [0, height)` に切り取る。`color` と `contiguous` はレイヤーのタイルから作るので、文書の外に出ることがある。

## SEL-1 色域・連続領域の SDK 公開

### 実装前の状態

- Rust：`generated_selection`（`crates/alto-engine/src/selection.rs`）が `kind` の `color` と `contiguous` を処理する。読むキーは次のとおり。
  - 共通：`layer`（ノード ID の文字列。必須）、`tolerance`（整数 0〜255、既定 0。RGBA の各チャンネルの差の最大値で比べる）、`mode`（`replace`／`add`／`subtract`／`intersect`、既定 `replace`）。
  - `contiguous`：`x`、`y`（整数。文書座標の起点。必須。レイヤー外なら `"Seed is outside the layer"`）。4近傍の走査線塗りつぶし。
  - `color`：`r`、`g`、`b`、`a`（整数 0〜255。省略時は 0、0、0、255）。レイヤーのタイルがある所だけを走査するので、タイルの無い透明領域は選ばれない。
- 条件：対象は `NodeKind::Raster` のみ。レイヤーのタイル数は 1024 以下（`MAX_SELECTION_SOURCE_TILES`）。レイヤー自身、親グループ、ルートを合わせた変形（`Engine::node_world`）の線形部分が単位行列でない（回転・拡縮・反転がある）と `"Color selection requires a translation-only layer"` で拒否する。平行移動は整数に丸めて反映する。
- 結果：選択のタイルが4096を超えると拒否する。結果が空のとき、`SetSelection { selection: None }` を返す（下の「空の結果」を参照）。
- SDK：`SelectionSpec.kind` は `"rectangle" | "ellipse" | "lasso" | "all" | "invert" | "clear"` のみ（`packages/alto-sdk/src/document.ts`）。`color` と `contiguous` は型の外で、TypeScript からは呼べない。
- 試験（Rust、`crates/alto-engine/src/selection.rs`）：`contiguous_selection_covers_a_layer_larger_than_the_old_pixel_cap`、`contiguous_selection_follows_walls_and_stops_at_missing_tiles`、`color_selections_use_the_whole_transform_chain`。SDK からの試験は無い。

### 11-04 からの変化

- 旧 `m5/mod.rs` は `crates/alto-engine/src/selection.rs` に改名された（4121d69。内容は同一）。旧設計書の `m5/mod.rs` の記述はすべてこちらに読み替える。
- `SelectionSpec` と `generated_selection` の中身は変化なし。

### 設計

`SelectionSpec` を判別共用体にする。Rust が読むキーとそろえる。

```ts
export type SelectionMode = "replace" | "add" | "subtract" | "intersect";

export type SelectionSpec =
  | {
      kind: "rectangle" | "ellipse";
      mode?: SelectionMode;
      x?: number; y?: number; width?: number; height?: number;
      feather?: number;
    }
  | { kind: "lasso"; mode?: SelectionMode; points: [number, number][]; feather?: number }
  | { kind: "all" | "invert"; mode?: SelectionMode }
  | { kind: "clear" }
  | {
      kind: "color";
      layer: NodeId;
      /** 0..255 の整数。 */
      r: number; g: number; b: number;
      /** 0..255 の整数。既定 255。 */
      a?: number;
      /** 0..255 の整数。既定 0。 */
      tolerance?: number;
      mode?: SelectionMode;
    }
  | {
      kind: "contiguous";
      layer: NodeId;
      /** 文書座標の起点（整数）。 */
      x: number; y: number;
      tolerance?: number;
      mode?: SelectionMode;
    };
```

- 既存の型で `points` だけは lasso にしか意味が無いので、`lasso` の枝に移す。Rust は `kind` ごとに必要なキーだけを読む。型を狭めるので、`kind: "rectangle"` に `points` を渡すコードは型エラーになる（実行時は今も無視される）。API 報告を更新する。
- `r`、`g`、`b` は Rust では省略が 0 になる。SDK では必須にして、省略の事故を型で防ぐ。
- `clear` は `mode` を取らない（Rust は読まない）。
- 数値の読み方の現状（`generated_selection`）：`r`、`g`、`b` は `as_u64().unwrap_or(0) as u8`、`a` は `as_u64().unwrap_or(255) as u8`。小数や負数、文字列は既定値（`r`、`g`、`b` は 0、`a` は 255）に落ち、256以上の整数は `as u8` で黙って下位8bitになる（300 は 44）。`x`、`y` は `as_i64().ok_or_else(..)` なので小数は `"Seed required"` で既に失敗するが、`i32` を超える整数は `as i32` で黙って切り捨てられる。`tolerance` は 255 を超えると既に拒否される（小数は 0 に落ちる）。案：`r`、`g`、`b`、`a` は整数 0〜255、`x`、`y` は `i32` に収まる整数でなければ `INVALID_ARGUMENT`（`"Color components must be integers 0..255"`、`"Seed must be an integer"`）にする（既定：直す。「黙って別の値になる」を除くだけで、有効な入力の結果は変わらない）。
- 空の結果：現状は `color` と `contiguous` の結果が空のとき `None`（選択なし＝全体が編集できる）にする。長方形などの空の結果は `Some`（空の選択）で、塗りは `EMPTY_SELECTION` になる。同じ「何も選ばれなかった」が別の意味になっている。案：`color`／`contiguous` も `Some(空)` にそろえる（`selection: Some(out)`）。結果が変わる既存の挙動なので、利用者の決定が要る。既定は「そろえる」。そろえない場合は、SDK の文書コメントに「一致が無いと選択が解除される」と書く。
  - 決定に含める影響：`invert` は `None` を「選択なし」として扱い、`1.0 - 1.0` で空の結果にする（`"invert" => 1.0 - if previous.is_some() { prior } else { 1.0 }`）。したがって、一致が無かった `color` の後の `invert` は、今は空の選択（`None` を反転）、そろえた後は全体の選択（`Some(空)` を反転）になる。`mask_for_node` は `None` を全体 255 のマスクにする。「`None` は全体が編集できる」と「`None` の `invert` は空」は今も食い違っている（直すかどうかも同じ決定に含め、既定では `invert` の現状を変えない）。
  - そろえた場合に変わる試験・利用：`color_selections_use_the_whole_transform_chain` など、結果が空になりうる既存の試験の `unwrap()` を確かめる。
- 変形つきレイヤーの扱いは今の制限を保つ（回転・拡縮があるレイヤーは拒否）。SDK の文書コメントに「`layer` は平行移動のみのラスターレイヤー」と書く。変形つきレイヤーへの対応は項目にしない。
- 上限は変えない。

### 形式と通信

なし。`select` の `spec` は `unknown` のまま、選択は R8 の `TileMap` のまま。`format_version`、`PROTOCOL`、描画の版は上げない。SDK の公開型が変わるので `packages/alto-sdk/api/alto-engine.api.md` を更新する。

- 互換：`SelectionSpec` は `@public` の型（API 報告に載る）。`r`、`g`、`b` を必須にし、`kind` と `points` を狭めるので、型の上では破壊的変更になる。TypeScript SDK は 0.x の系列なので、`RELEASING.md` の版の方針に従って版を上げ、変更履歴に書く手順を作業手順に含める（版の上げ方は利用者の決定）。

### 作業手順

**Files:**
- Modify `packages/alto-sdk/src/document.ts`（`SelectionSpec`、`SelectionMode` の型）
- Modify `packages/alto-sdk/src/index.ts`（`SelectionMode` を公開する場合）
- Modify `packages/alto-sdk/api/alto-engine.api.md`（`pnpm --dir packages/alto-sdk api` で更新）
- Modify `crates/alto-engine/src/selection.rs`（整数検査と、空の結果の決定を実装する場合）
- Modify `tests/e2e/m5-tests.js`

**試験：**
- Rust では不正な色成分・許容差・起点を拒否し、範囲外へ変換される起点も文書を変えずに拒否する。一致なしと差し引きで空になった場合は R8 の空選択を保持し、塗りを `EMPTY_SELECTION` で拒否する。
- ブラウザでは、連続領域の境界と Undo、離れた同色領域と許容差、差し引き、空の結果、回転レイヤーを拒否したときの文書保全を確認する。
- SDK のビルドと公開 API 報告を検査する。公開型の変更は RELEASING に記録し、SDK の版はリリース準備時に決める。

### 依存

- 先に要る項目：なし。
- 後に続く項目：SEL-2 と SEL-3 は `SelectionSpec` に枝を足すので、SEL-1 の型の後に入れる（同じ型を3つの PR が順に変える）。

## SEL-2 拡大、縮小、境界

### 現状

- 加工の手段は無い。`selection_ops` の `kind` は `rectangle`、`ellipse`、`lasso`、`all`、`invert`、`clear`、`color`、`contiguous` のみ。それ以外は `"Unknown selection shape"`（`INVALID_ARGUMENT`）。
- 選択の読み出し：`decode(map, store)` が R8 タイルを `BTreeMap<TileKey, Arc<TileData>>`（`Decoded`）に展開し、`sample(&Decoded, x, y)` が1画素を読む（どちらも `crates/alto-engine/src/selection.rs`）。範囲外とタイルの無い所は 0。
- タイルの書き込み：`Engine::put_tile` は全画素が 0 のタイルを保存せず `None` を返す（`crates/alto-engine/src/lib.rs`）。
- 注意：`selection.rs` の `Coverage` は1画素1ビットの集合で、柔らかい選択（0〜255）を持てない。SEL-2 では使わず、`Decoded` を読む。
- 検証：`SetSelection` は `validate_mask`（`crates/alto-doc/src/node.rs`）と、`transact` から呼ばれる `Engine::validate_mask_plane`（`crates/alto-engine/src/lib.rs`。R8 であることと、タイルがストアにあることだけを見る）を通る。タイル数の上限は検査しないので、4096の保証は `selection.rs` の中で守る。

### 11-04 からの変化

変化なし（`selection.rs` の中身は `m5/mod.rs` と同一。改名のみ）。

### 設計

```ts
{ kind: "modify"; operation: "grow" | "shrink" | "border" | "smooth"; radius: number }
```

- `radius`：整数 1〜64（`MAX_MODIFY_RADIUS = 64`（新規）。ぼかし幅の上限と同じ）。整数でない、範囲外は `INVALID_ARGUMENT`（`"Selection radius must be an integer 1..64"`）。
- `operation` が上の4つ以外は `INVALID_ARGUMENT`（`"Unknown selection modification"`）。`mode` は取らず、読まない。`selection_ops` の `mode` の検査（`clear`、`color`、`contiguous` の分岐より後、形の分岐より前）に届く前に、`modify` を `clear` と同じ位置で分岐させる。したがって `mode: "bogus"` もエラーにならず無視される（SDK の型には `mode` が無いので、型の上では渡せない）。
- 現在の選択が `None` のとき `INVALID_ARGUMENT`（`"There is no selection to modify"`）。`Some` で空の選択は、結果も空の選択（`Some` の空）で、エラーにしない。
- 入力は現在の選択の R8（0〜255）。全画素の外側（タイルの無い所、文書の外）は 0 として扱う。

**アルゴリズム（整数、CPU）。** 出力タイルごとに、近傍のタイルから `(256 + 2r) × (256 + 2r)` の窓（`u8`）を作り、窓の中だけで計算する。`r ≤ 64 < 256` なので必要な入力タイルは出力タイルと周囲の8タイルだけ。窓は最大 `384 × 384` で、全体の大きなバッファは作らない。

- 構造要素：半径 r の円盤。行ごとの半幅の表 `w[dy] = isqrt(r*r - dy*dy)`（`dy = -r..=r`、整数）。`r = 1` は十字になる。
- `grow`：灰色膨張。`out(x, y) = max over dy of rowmax(y+dy, x, w[dy])`。ぼかしのある選択の柔らかさを保つ。
- `shrink`：灰色収縮。`min` で同様。選択の外は 0 なので、選択が文書の端に接する所も縮む。
- `border`：`grow − shrink`（飽和減算）。幅はおよそ `2r`。
- `smooth`：`(2r+1) × (2r+1)` の箱平均 `(sum + n/2) / n`（`n = (2r+1)²`、整数）。2 値化しない（柔らかいまま）。円盤ではなく正方形（行ごとの移動和で半径に依存しない計算量）。
- 行ごとの最大・最小：窓の各行について、必要な半幅（相異なる `w[dy]`、最大 r+1 通り）ごとに van Herk／Gil-Werman 法の横方向の最大（最小）を1回ずつ作る。縦は `dy` ごとに対応する行の値を取り、最大（最小）を取る。画素あたりの縦の演算は `2r+1` 回なので、`grow`／`shrink` は半径に依存しない計算量にはならない（半径に比例する）。

**出力の範囲と上限。**
- 出力は文書の範囲 `[0, width) × [0, height)` に切り取る。文書の外は 0。長方形などの既存の生成と同じ範囲に揃える。
- 出力タイルの候補：`grow`／`border`／`smooth` は入力の占有タイルとその8近傍を、文書の範囲に切り取った集合（`r ≤ 64 < 256` なので影響は隣のタイルまで）。外接矩形は使わない（離れた2隅の小さな選択が、間の全タイルを候補にしてしまう）。`shrink` は入力の占有タイルの集合。候補が4096を超えるなら、計算の前に `INVALID_ARGUMENT`（`"Selection/mask exceeds 4096 tiles"`）で拒否する。文書の範囲に切り取るので、文書全体を覆う選択の `grow` は候補が増えず拒否されない。
- 作業量の上限 `MAX_MODIFY_WORK: u64 = 1 << 31`（新規。既定値で、測定で調整する）：作業量 = `候補タイル数 × 65536 × k` が `MAX_MODIFY_WORK`（新規） を超えるなら、計算の前に `INVALID_ARGUMENT`（`"Selection modification is too large; reduce the radius or the area"`）で拒否する。`k` は `grow`／`shrink` が `2r + 1`、`border` が `2 × (2r + 1)`（膨張と収縮の2回）、`smooth` が 4（横と縦の移動和）。操作ごとに実際の演算の形に合わせ、1つの定数で足りるように式を分ける。
- 結果のタイルは `put_tile` で保存し、全画素 0 のタイルは作らない。結果にタイルが無いときも `SetSelection { selection: Some(空の TileMap) }` を返す。`None` にしない（「空の選択」と「選択なし」を区別する既存の規則。`shrink` で消えた選択は、どこにも描けない）。
- 計算は検証してから行い、エラーのとき文書を変えない。保存したタイルは `Op` が適用されるまで文書から参照されない。

**Worker で動く点。** 計算は Worker の WASM の中で動き、UI スレッドを止めない。ただし Worker の他の要求はこの `select` が終わるまで待つ。作業量の上限でそれを抑える（SEL-3 にも同じ形の上限を置く）。

### 形式と通信

なし。`select` の `spec` に新しい `kind` を足すだけで、`PROTOCOL`、`format_version`、描画の版は上げない。SDK の公開型（`SelectionSpec`）が変わるので API 報告を更新する。

### 作業手順

**Files:**
- Modify `crates/alto-engine/src/selection.rs`（`kind == "modify"` の分岐、`modify_selection`（新規）、窓の計算）
- Modify `packages/alto-sdk/src/document.ts`（`SelectionSpec` に枝）
- Modify `packages/alto-sdk/api/alto-engine.api.md`
- Modify `tests/e2e/m5-tests.js`

**Interfaces:** Produces `fn modify_selection(engine: &mut Engine, h: DocHandle, spec: &Value) -> Result<Vec<Op>, EngineError>`（新規、`selection.rs`）、`fn modify_window(window: &[u8], side: usize, operation: Modify, radius: u32) -> Vec<u8>`（新規。窓を受け取り、窓の中央 256×256 を返す純粋な関数。試験しやすくする）、`enum Modify`（新規）。Consumes `decode`、`sample`、`Engine::put_tile`。

- [ ] Rust 試験（`crates/alto-engine/src/selection.rs` の `mod tests`。`layer_doc` と `commit` を使う）：
  - `grow_then_shrink_of_a_rect_is_the_rect`：長方形を `grow` して `shrink` すると、辺の中央が元の値に戻る。角は丸まるので、角を除いて確かめる。
  - `border_is_empty_inside_and_outside`：`border` の内側の深い所と外側の遠い所が 0。縁の近くが 0 より大きい。
  - `grow_crosses_tile_boundaries`：タイルの境の近くの小さな選択を `grow` すると、隣のタイルにタイルが増え、画素が連続している。
  - `shrink_to_nothing_clears_the_selection`：消えるまで縮めると `Some` で空の選択になり、`None` ではない。その後の `image.fill` 相当が `EMPTY_SELECTION` になる。
  - `modify_without_selection_is_invalid`：選択なしで `INVALID_ARGUMENT`。
  - `modify_keeps_soft_edges`：ぼかしのある長方形で、`grow` の各画素が入力以上、`shrink` の各画素が入力以下、どちらも 0 と 255 以外の値が残る。`smooth` の結果も 0 と 255 以外の値を含む。
  - `modify_stays_inside_the_document`：端に接する選択の `grow` が文書の外にタイルを作らない。
  - `modify_radius_and_operation_are_validated`：`radius` が 0、65、1.5、省略、`operation` が不明で `INVALID_ARGUMENT`。いずれも文書を変えない。
  - `grow_past_the_tile_limit_is_refused_atomically`：互いに離れた500個ほどの占有タイル（占有タイル＋8近傍が4096を超える）の `grow` が、計算の前に拒否され、`document.selection` もタイルストアも変わらない。
  - `grow_of_a_full_canvas_selection_is_not_refused`：64×64タイルの文書の全体選択の `grow` は、候補が文書の範囲に切り取られて4096以下になり、拒否されず、結果が入力と同じ。
  - `modify_of_two_far_corners_uses_only_occupied_tiles`：離れた2隅の小さな選択の `grow` が、候補を占有タイルとその8近傍だけに保ち、間のタイルを作らない。
  - `modify_of_an_empty_selection_stays_empty`：`Some(空)` への各 `operation` が `Some(空)` を返す（エラーにも `None` にもならない）。
  - `modify_window_distinguishes_disc_from_square`：1画素を `grow`（`radius = 3`）して、円盤の外の角 `(3, 3)` が 0、`smooth` では 0 でない。`grow_then_shrink_of_a_rect_is_the_rect` は角を除くので、円盤と正方形を見分けられない。
  - `modify_work_limit_is_refused_before_any_work`：作業量の上限を超える入力が計算の前に拒否される。`border` が同じ入力の `grow` の2倍の作業量で数えられる（`smooth` は半径に依存しない）。
  - `modify_window_matches_a_brute_force_reference`：小さな窓で、各 `operation` と `radius` 1、2、5、8 の結果が、素朴な二重ループ（円盤または正方形を直接走査）と全バイト一致する。総当たりは半径8までに限る（半径64では1回あたり約19億回の比較になる）。`radius = 64` は、窓の端での最大・最小が両側の窓の外を 0 として扱うことだけを、小さな入力で確かめる。
  - `modify_is_one_undo_step`：`transact` 後に Undo で元の選択に戻り、Redo で同じタイルのハッシュになる。ジャーナルを通した再オープンでも同じ（共通規則4）。
- [ ] ブラウザ試験 `selection modify grows, shrinks and borders through the SDK`（`tests/e2e/m5-tests.js`）：SDK の `doc.selection.apply({ kind: "modify", ... })` を4つの `operation` で呼び、`getSnapshot().selection` が変わる。選択なしでの `modify` が `INVALID_ARGUMENT`。
- [ ] 実装する。窓の作成、横方向の van Herk、縦の結合、箱平均の順に作り、各段で上の試験を通す。
- [ ] 完了条件：4つの `operation` が動き、上の試験が通り、既存の選択の試験とブラウザ試験が変わらない。
- [ ] 検証：`cargo test -p alto-engine selection`、`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`pnpm --dir packages/alto-sdk api:check`、`node scripts/verify-browser.mjs --functional`。PR を出して opencode でレビューする。

**測定で形が変わる点。** `MAX_MODIFY_WORK` は、Chrome（SwiftShader と Metal）の Worker で `radius = 64` の `grow` が、拒否されない最大の入力でも他の要求を著しく待たせない値に調整する。待たせすぎる場合は、`grow`／`shrink` を円盤の厳密な計算から、八角形の分解（正方形と菱形の合成。画素あたりの演算が半径に依存しない）へ変える案を出して、利用者に決めてもらう。この変更は形（円か八角形か）を変えるので、先に決めずに測定結果を示す。

### 依存

- 先に要る項目：SEL-1（`SelectionSpec` の型）。
- 後に続く項目：SEL-3（`feather` の実装が `smooth` の箱平均を再利用する）。

## SEL-3 パスからの選択

### 現状

- 注入の形：`pub type VectorRasterizer<'a> = &'a dyn Fn(&[StyledPath], FixedAffine, [i64; 4]) -> Result<Vec<[u16; 4]>, String>`（`crates/alto-engine/src/canvas.rs`）。`window` は `[x, y, w, h]`（世界座標の画素）、戻りは窓の各画素のプレマルチプライ Q14 の RGBA。`alto-engine` が GPU に依存しないための形。
- 実体：`alto_gpu::raster_q8::vector_pixels_q14(paths, world, window)`（`crates/alto-gpu/src/raster_q8.rs`）。`crates/alto-wasm/src/image_versions.rs` の `layer_edit` が、これを `VectorRasterizer` として `prepare_layer_edit` に渡している。
- 同じ実体の `Prepared`（`crates/alto-gpu/src/raster_q8.rs`）は、コマンドを一度だけ正規化して多くの窓へ描くための型で、`rasterize_crop_prepared` が窓ごとに使う。`vector_pixels_q14` は呼ぶたびに `map_path` と正規化をやり直す。
- 使われ方：`vector_tiles`（`crates/alto-engine/src/canvas.rs`）が、タイルごとに `[tx*256, ty*256, 256, 256]` の窓で呼び、α を `((a * 255 + 8192) / 16384)` で8bit にする。エラー文字列は `UNSUPPORTED` にする。
- パスの型：`alto_vector::PathData`、`PathCommand`（`move`／`line`／`quad`／`cubic`／`close`）、`FillRule`（`non_zero`／`even_odd`）、`StyledPath`（`crates/alto-vector/src/lib.rs`）。`PathData::validate` が 4096 命令、座標 ±1048576、部分パスの規則（`move` で始める）を検査する。
- SDK の型：`PathData`、`FillRule`（`packages/alto-sdk/src/types.ts`）。
- `selection_ops` は今、ラスタライザを受け取らない。呼び出し元：`crates/alto-wasm/src/lib.rs` の `select`、`crates/alto-engine/examples/create-masks-sample.rs`（3か所）、`selection.rs` の試験。

### 11-04 からの変化

- `m5/mod.rs` は `selection.rs` に改名された。
- `VectorRasterizer` の定義と `image_versions.rs` の呼び出しは変化なし。

### 設計

```ts
{ kind: "path"; path: PathData; fill_rule?: FillRule; feather?: number; mode?: SelectionMode }
```

- `path`：`PathData`（SDK の `types.ts`）。座標は文書座標。Rust は `serde_json::from_value::<alto_vector::PathData>` で読み、`validate()` を通す。失敗は `INVALID_ARGUMENT`（`"Path selection requires valid path data"`）。コマンドが空、または `move` だけで面積が無いパスは空の選択になる（エラーにしない）。
- `fill_rule`：`"non_zero"`（既定）か `"even_odd"`。
- `feather`：0〜64（既存の `feather` と同じ範囲と検査）。
- `mode`：既存の4つ。既存の結合式（`add`、`subtract`、`intersect`）をそのまま使う。

**ラスタライザの注入。** `selection_ops` に引数を足す。

```rust
pub fn selection_ops(
    &mut self,
    h: DocHandle,
    spec: &Value,
    rasterizer: PathCoverage<'_>,
) -> Result<Vec<Op>, EngineError>
```

注入する型の注意：`VectorRasterizer` は `StyledPath` の列を受け、呼ぶたびに正規化をやり直すので、4096タイルで4096回の正規化になる。案（既定）：`Prepared` を使う窓のまとめ呼び出し用に、`canvas.rs` に注入の型 `PathCoverage<'a>`（新規：`&'a dyn Fn(&PathData, FillRule, &[[i64; 4]]) -> Result<Vec<Vec<u8>>, PathCoverageError>`、窓ごとの8bit被覆を返す）を足し、`alto-wasm` 側で `Prepared::new` を一度だけ呼んで `rasterize_crop_prepared` を窓ごとに使う。8bit への換算は `raster_q8` の被覆の単位を確かめて決める（実装時）。`PathCoverageError`（新規）は `TooComplex` と `OutOfRange` を持つ。代案：`VectorRasterizer` をそのまま使い、正規化の繰り返しを作業量の上限に含める。以下の `selection_ops` のシグネチャ、手順、閉包は、既定の案に合わせて読み替える。

`alto-wasm` の `select` は `layer_edit` と同じ閉包（`alto_gpu::raster_q8::vector_pixels_q14` を呼び、`"the vector is outside the renderer's coordinate range"` に変換する）を渡す。閉包は `crates/alto-wasm/src/image_versions.rs` から関数に切り出して2か所で使う。他の `kind` は `rasterizer` を使わない。

**アルゴリズム。**
1. 範囲：パスの制御点の外接矩形を求める。`feather` があれば両側に `feather / 2` 広げ、文書の範囲 `[0, width) × [0, height)` に切り取る。`mode != "replace"` なら既存の選択のタイルの範囲も含める（既存の `selection_ops` と同じ）。`checked_bounds` で4096タイルを超えるなら `INVALID_ARGUMENT`（`"Selection/mask exceeds 4096 tiles"`）。
2. タイルごと：パスの制御点の外接矩形と重ならないタイルは、ラスタライザを呼ばずに飛ばす。残りは `window = [tx*256, ty*256, 256, 256]` の被覆を `rasterizer` から得る（`world` は恒等）。これが `amount`（8bit を 0〜1 にして使う）。文書の外の画素は 0。代案（`VectorRasterizer` を使う場合）：`StyledPath { data, fill: Some([1.0, 1.0, 1.0, 1.0]), stroke: None, fill_rule }` を1つ渡し、α（Q14）を `(a * 255 + 8192) / 16384` で8bitにする。
3. `feather == 0`：`amount` をそのまま既存の結合式へ渡す。長方形パスは整数座標で `rectangle` の `feather == 0` の結果と同じ値になる（画素の中心の内外判定と、端の画素の被覆が一致する）。
4. `feather > 0`：パスの被覆（手順2の結果）を、SEL-2 の `smooth` の箱平均（半径 `round(feather / 2)`、最小 1）で柔らかくしてから結合する。直線の縁では幅 `2r + 1` の線形の傾きになり、`rectangle` の `feather` の傾き（幅 `feather`）に近いが、画素は一致しない。一致は約束しない。SEL-2 より前には入れず、前に出す場合は `feather > 0` を `INVALID_ARGUMENT`（`"Path selection feather is not available"`）にする。
5. 結合式の結果を R8 のタイルにして `put_tile` する。全画素 0 のタイルは作らない。結果にタイルが無いときは `Some(空の TileMap)`（既存の長方形などと同じ。`None` にしない）。

**上限とエラー。** パスは `PathData::validate` の上限（4096 命令、座標 ±1048576）、タイルは4096。作業量は、候補タイル × 65536 画素の被覆の計算に、タイルごとの曲線の分割（命令数に比例）を足したもの。`MAX_PATH_SELECTION_WORK: u64 = 1 << 31`（新規。既定値で、測定で調整する）を置き、`候補タイル数 × (65536 + 命令数 × 64)` がそれを超えたら、計算の前に `INVALID_ARGUMENT`（`"Path selection is too large; reduce the path or the area"`）で拒否する（`64` も既定値）。最大の4096タイルでも被覆の画素数は約2.7億で、SEL-2 の上限より小さいが、曲線の分割が加わるのでこの上限が要る。ラスタライザの失敗：`OutOfRange`（新規）（座標が範囲外）は `UNSUPPORTED`、`TooComplex`（新規）（曲線の予算超過。`raster_q8` は `CurveRange` として扱う）は `INVALID_ARGUMENT`（`"Path is too complex"`）にする。`vector_pixels_q14` は曲線の予算を超えたパスを黙って飛ばす（空になる）ので、`VectorRasterizer` を使う代案では、複雑すぎるパスは空の選択になる。計算は検証してから行い、エラーのとき文書は変わらない。

**ベクターの画素との一致。** 同じ `raster_q8` の塗りの被覆を通すので、単一のパスの塗りだけは、そのパスをベクターレイヤーに描いたときの塗りの被覆と同じ値になる。複数のパスの合成、線（ストローク）、曲線の予算を超えて描画で飛ばされるパスは、選択には反映されない。レンダラー側の描画は変えない。描画の3実装には関係しない（選択は CPU の R8）。

### 形式と通信

なし。選択は R8 のまま、`spec` の `kind` が増えるだけ。`format_version`、`PROTOCOL`、描画の版は上げない。`selection_ops` の Rust のシグネチャが変わるが、公開の API ではない。SDK の公開型が変わるので API 報告を更新する。

### 作業手順

**Files:**
- Modify `crates/alto-engine/src/selection.rs`（`kind == "path"` の分岐、`selection_ops` の引数、パスの被覆の計算）
- Modify `crates/alto-engine/src/canvas.rs`（`PathCoverage` の型を足す場合）
- Modify `crates/alto-wasm/src/lib.rs`（`select` が閉包を渡す）
- Modify `crates/alto-wasm/src/image_versions.rs`（閉包を関数に切り出す）
- Modify `crates/alto-engine/examples/create-masks-sample.rs`（`selection_ops` の呼び出し3か所）
- Modify `packages/alto-sdk/src/document.ts`（`SelectionSpec` に枝）、`packages/alto-sdk/api/alto-engine.api.md`
- Modify `tests/e2e/m5-tests.js`

**Interfaces:** Consumes `VectorRasterizer`（`crates/alto-engine/src/canvas.rs`。代案の場合）、`alto_gpu::raster_q8::Prepared`、`alto_vector::PathData::validate`。Produces `selection_ops` の新しいシグネチャ、`PathCoverage`（新規）、`PathCoverageError`（新規）、`MAX_PATH_SELECTION_WORK`（新規）。

- [ ] 試験用のラスタライザ：`selection.rs` の `mod tests` は `alto-gpu` に依存できない（`alto-engine` が GPU に依存しないため）。単位が整数の長方形と、半分だけ被覆する縁を返す小さな固定の閉包（`StyledPath` の長方形だけを扱う）を試験の中に書く。実際の `raster_q8` との一致はブラウザ試験で確かめる。
- [ ] Rust 試験（`selection.rs`）：
  - `path_selection_matches_rectangle_selection`：整数座標の長方形パスの `path` 選択が、同じ長方形の `rectangle` 選択と同じタイルハッシュ。
  - `even_odd_hole_is_not_selected`：外側と内側の長方形のパスで、`even_odd` は内側が 0、`non_zero`（同じ向き）は内側も 255。
  - `path_selection_respects_add_and_subtract`：`add`、`subtract`、`intersect` が既存の長方形との結合と同じ結果。
  - `path_selection_is_clipped_to_the_document`：文書の外へはみ出すパスがタイルを文書の外に作らない。
  - `path_work_limit_is_refused_before_any_work`：4096命令の複雑なパスを大きな範囲に広げると、ラスタライザを呼ぶ前に拒否される（呼び出し回数の数える閉包で確かめる）。
  - `huge_path_is_refused_by_the_tile_limit`：座標が大きく、4096タイルを超えるパスが `INVALID_ARGUMENT` で拒否され、文書とタイルストアが変わらない。
  - `invalid_path_is_refused_atomically`：4097 命令、座標が ±1048576 の外、`move` で始まらないパスが `INVALID_ARGUMENT`。
  - `empty_path_gives_an_empty_selection`：コマンドが空のパスで `Some` の空の選択になり、`EMPTY_SELECTION` で塗りが拒否される。
  - `path_feather_softens_the_edge`（SEL-2 の後）：`feather > 0` で縁の値が 0 と 255 の間になり、内側の深い所は 255。
  - `path_selection_is_one_undo_step`：1回の Undo で元に戻る。
  - `path_selection_skips_tiles_outside_the_path_bounds`：外接矩形の外のタイルでラスタライザが呼ばれない。
  - 既存の `selection_ops` の試験を新しいシグネチャ（エラーを返す閉包を渡す）に直す。
- [ ] ブラウザ試験 `a path selection matches the vector layer's coverage`（`tests/e2e/m5-tests.js`）：曲線を含む単一のパスを `doc.selection.apply({ kind: "path", path })` で選び、同じパスを塗りだけ（線なし）のベクターレイヤーに描いたときの α（`at(engine, x, y)`）と、選択の値が 8bit の丸めの範囲で一致する。`fill_rule: "even_odd"` の穴が選ばれない。
- [ ] ブラウザ試験 `a path selection is refused when it is too large`：巨大なパスが `INVALID_ARGUMENT`、選択が変わらない。
- [ ] 完了条件：`path` の選択が動き、上の試験が通る。既存のベクター・選択・描画のハッシュ（`tests/parity/` の既存行）が変わらない（共通規則1）。
- [ ] 検証：`cargo test -p alto-engine selection`、`cargo build -p alto-wasm`、`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`pnpm --dir packages/alto-sdk api:check`、`node scripts/verify-browser.mjs --functional`。PR を出して opencode でレビューする。

### 依存

- 先に要る項目：SEL-1（型）。`feather > 0` の実装は SEL-2。IMG-3（05）は `crates/alto-wasm/src/image_versions.rs` の同じ閉包（`Rasterizers`（新規））を変えるので、IMG-3 を先に入れ、本項目はその後に引数を足す（00-ROADMAP の重なりの表）。
- 後に続く項目：なし。VEC-1（グラデーション）、VEC-4（vello）はベクターの描画を変えるが、選択は `raster_q8` の被覆（塗りの α）だけを使うので、塗りの色やグラデーションは影響しない。`raster_q8` の被覆の計算が変わる項目（VEC-2 の破線は線だけ）が現れたら、この選択の試験を再確認する。

## SEL-4 被写体選択

除外（AI 機能）。実装しない。記録は [12-collab-ai.md](12-collab-ai.md)。

## Review Focus

- キャンバスより外へ広がる選択の扱い。`modify` は文書の範囲に切り取り、長方形などの既存の生成と同じ範囲に揃える。`color`／`contiguous` はレイヤーのタイルから作る既存の挙動を変えない。
- ぼかしのある選択への `shrink` で、値が飛ばないこと（`min` で、元の値以下）。
- 選択なし（`None`）と「空の選択」（`Some` の空）を区別する既存の規則（空はどこにも描けない。`EMPTY_SELECTION`）を、`shrink` の結果と `path` の結果に適用すること。`color`／`contiguous` の結果が空のとき `None` になる現状は、規則と食い違う（SEL-1 の「空の結果」）。
- 大半径の `modify` が Worker で実行されること（UI スレッドは止まらない）と、作業量の上限で他の要求を待たせすぎないこと。
- レイヤーの `color` 選択で、レイヤー変形を持つ場合の扱いを既存の `offset` の制限（平行移動のみ）と揃えること。
- `selection_ops` のシグネチャ変更（SEL-3）で、`create-masks-sample.rs` と既存の試験が全部直っていること。
