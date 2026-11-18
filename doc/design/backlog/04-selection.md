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

### 実装前の状態

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

**アルゴリズム（整数、CPU）。** 出力タイルごとに、近傍のタイルから `(256 + 2r) × (256 + 2r)` の窓（`u8`）を作り、窓の中だけで計算する。`r ≤ 64 < 256` なので必要な入力タイルは出力タイルと周囲の8タイルだけ。窓は最大 `384 × 384`。出力はタイル単位で保持し、全候補の処理が成功してからタイルストアへ書き込む。作業上限で保持できる出力画素列は最大約86 MiB。

- 構造要素：半径 r の円盤。行ごとの半幅の表 `w[dy] = isqrt(r*r - dy*dy)`（`dy = -r..=r`、整数）。`r = 1` は十字になる。
- `grow`：灰色膨張。`out(x, y) = max over dy of rowmax(y+dy, x, w[dy])`。ぼかしのある選択の柔らかさを保つ。
- `shrink`：灰色収縮。`min` で同様。選択の外は 0 なので、選択が文書の端に接する所も縮む。
- `border`：`grow − shrink`（飽和減算）。幅はおよそ `2r`。
- `smooth`：`(2r+1) × (2r+1)` の箱平均 `(sum + n/2) / n`（`n = (2r+1)²`、整数）。2 値化しない（柔らかいまま）。円盤ではなく正方形（行ごとの移動和で半径に依存しない計算量）。
- 行ごとの最大・最小：窓の各行について、必要な半幅（相異なる `w[dy]`、最大 r+1 通り）ごとに van Herk／Gil-Werman 法の横方向の最大（最小）を1回ずつ作る。縦は `dy` ごとに対応する行の値を取り、最大（最小）を取る。画素あたりの縦の演算は `2r+1` 回なので、`grow`／`shrink` は半径に依存しない計算量にはならない（半径に比例する）。

**出力の範囲と上限。**
- 出力は文書の範囲 `[0, width) × [0, height)` に切り取る。文書の外は 0。長方形などの既存の生成と同じ範囲に揃える。
- 出力タイルの候補：`grow`／`border`／`smooth` は入力の占有タイルとその8近傍を、文書の範囲に切り取った集合（`r ≤ 64 < 256` なので影響は隣のタイルまで）。外接矩形は使わない（離れた2隅の小さな選択が、間の全タイルを候補にしてしまう）。`shrink` は入力の占有タイルの集合。候補が4096を超えるなら、計算の前に `INVALID_ARGUMENT`（`"Selection/mask exceeds 4096 tiles"`）で拒否する。文書の範囲に切り取るので、文書全体を覆う不透明な選択の `grow` では、全面のタイルを確認して既存のタイルを再利用する。文書外のタイルは結果から除く。
- 作業量の上限 `MAX_MODIFY_WORK: u64 = 1 << 28`：作業量 = `候補タイル数 × 65536 × k` が `MAX_MODIFY_WORK`（新規） を超えるなら、計算の前に `INVALID_ARGUMENT`（`"Selection modification is too large; reduce the radius or the area"`）で拒否する。`k` は `grow`／`shrink` が `2r + 1`、`border` が `2 × (2r + 1)`（膨張と収縮の2回）、`smooth` が 4（横と縦の移動和）。操作ごとに実際の演算の形に合わせ、1つの定数で足りるように式を分ける。
- 結果のタイルは `put_tile` で保存し、全画素 0 のタイルは作らない。結果にタイルが無いときも `SetSelection { selection: Some(空の TileMap) }` を返す。`None` にしない（「空の選択」と「選択なし」を区別する既存の規則。`shrink` で消えた選択は、どこにも描けない）。
- 計算は検証してから行い、エラーのとき文書を変えない。保存したタイルは `Op` が適用されるまで文書から参照されない。

**Worker で動く点。** 計算は Worker の WASM の中で動き、UI スレッドを止めない。ただし Worker の他の要求はこの `select` が終わるまで待つ。作業量の上限でそれを抑える（SEL-3 にも同じ形の上限を置く）。

### 形式と通信

なし。`select` の `spec` に新しい `kind` を足すだけで、`PROTOCOL`、`format_version`、描画の版は上げない。SDK の公開型（`SelectionSpec`）が変わるので API 報告を更新する。

### 作業手順

実装は `crates/alto-engine/src/selection/modify.rs` に置き、既存の `select` から呼び出す。SDK の公開型と API 報告を更新した。

**試験：**
- Rust で円盤と箱平均を画素ごとの直接計算と比較する。最大半径、タイルの継ぎ目、部分タイルの境界、文書外の入力、空選択と無効な入力を確認する。
- タイル上限と作業量上限による拒否、離れたタイルだけを候補にすること、全面選択の再利用、Undo・Redo・ジャーナル復旧で同じタイルに戻ることを確認する。
- ブラウザで SDK から4操作を実行し、半径64では非一様な31タイルを処理でき、32タイルを拒否することを確認する。

**測定：** Chrome 157 で半径64の `grow` を実行し、SDK の `selection.apply` が解決するまでを計測した。候補31タイルの横長の選択で、3回の測定で Metal は約296〜308 ms、SwiftShader は約299〜310 ms。入力形状と環境で時間は変わる。円盤を維持し、作業上限を `1 << 28` に設定した。

### 依存

- 先に要る項目：SEL-1（`SelectionSpec` の型）。
- 後に続く項目：SEL-3（`feather` の実装が `smooth` の箱平均を再利用する）。

## SEL-3 パスからの選択

`doc.selection.apply({ kind: "path", path, fill_rule?, feather?, mode? })` で、文書座標の Bézier パスの塗りを R8 の選択範囲に変換する。開いた部分パスも塗りに使うときは閉じる。塗り規則は `non_zero`（既定）または `even_odd`。選択の結合には既存の4モードを使う。空のパス、move のみ、文書外のパスは被覆が空になる。置換では R8 の空マスクを保持する。

`alto-wasm` の `select` は `PathCoverage` を注入する。レベル0のベクター塗りと同じ `raster_q8` の8bit被覆を使い、`Prepared` はパスごとに一度だけ作る。タイルを制御点の外接矩形とフェザーの半径で絞り、既存の選択との結合には疎なタイル集合を使う。フェザー（0〜64）は SEL-2 と同じ箱平均を半径 `round(feather / 2)`（正の幅では最小1）で使う。文書の外は平均前の被覆と出力の両方で0にする。

パスの構造・命令数・座標は `PathData::validate` で検査する。4096タイルを超える場合は描画前に拒否する。別の作業量上限（2^27）は、候補タイルの合成、広げた窓の被覆と箱平均、曲線の分割、命令ごとの走査を見積もる。窓ごとの走査にも上限を置き、`even_odd` の交差点用バッファを抑える。上限超過や曲線の予算超過は `INVALID_ARGUMENT`、ラスタライザの座標範囲エラーは `UNSUPPORTED`。失敗時に選択は更新しない。

実装は SEL-1 の SDK 型と SEL-2 の箱平均に依存する。パス用の注入は `select` に閉じているため、IMG-3 の `image_versions.rs` とは順序の制約がない。ファイル形式、プロトコル、描画の版は据え置く。

Rust の回帰試験は結合・Undo、フェザーとクリッピング、疎なタイル、空パス、描画前の上限検査を確認する。ブラウザ試験では、曲線と穴のあるパスを2タイルに描き、ベクターレイヤーと選択マスクの全画素のアルファを許容差1で比較する。

## SEL-4 被写体選択

除外（AI 機能）。実装しない。記録は [12-collab-ai.md](12-collab-ai.md)。

## Review Focus

- キャンバスより外へ広がる選択の扱い。`modify` は文書の範囲に切り取り、長方形などの既存の生成と同じ範囲に揃える。`color`／`contiguous` はレイヤーのタイルから作る既存の挙動を変えない。
- ぼかしのある選択への `shrink` で、値が飛ばないこと（`min` で、元の値以下）。
- 選択なし（`None`）と「空の選択」（`Some` の空）を区別する既存の規則（空はどこにも描けない。`EMPTY_SELECTION`）を、`shrink` の結果と `path` の結果に適用すること。`color`／`contiguous` の結果が空のとき `None` になる現状は、規則と食い違う（SEL-1 の「空の結果」）。
- 大半径の `modify` が Worker で実行されること（UI スレッドは止まらない）と、作業量の上限で他の要求を待たせすぎないこと。
- レイヤーの `color` 選択で、レイヤー変形を持つ場合の扱いを既存の `offset` の制限（平行移動のみ）と揃えること。
- `selection_ops` のシグネチャ変更（SEL-3）で、`create-masks-sample.rs` と既存の試験が全部直っていること。
