# 共同編集と AI 機能

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 共同編集を実装する。AI 機能は除外の記録として残す。

**Architecture:** README のとおり、画像や編集内容を外部へ送る処理は SDK 本体にない。共同編集の通信は別パッケージ `packages/alto-collab/`（新規）に置き、利用者の明示的な呼び出し（`host`、`join`）でのみ動かす。SDK 本体には通信を持たない取り付け口だけを足す。

状態：COLLAB-1 は実装の対象。AI-2、SEL-4、AI-3 は除外（2026-11-04、利用者の決定）。

## AI-2 外部画素とマスクの取り込み API（除外）

**設計。** 被写体選択と生成塗りつぶしの共通の土台。組み込む側がモデルを呼び、結果をエンジンへ渡す。

```ts
doc.selection.fromPixels({ x, y, width, height, data: Uint8Array /* R8 */, mode? }): Promise<void>
doc.image.replacePixels(node, { x, y, width, height, data: Uint8ClampedArray /* RGBA8 */, selection?: "use" | "ignore" }): Promise<ImageEditResult>
doc.exportRegionPixels({ region, source: "layer" | "composite", node? }): Promise<ImageData>
```

- `fromPixels`（新規）：`SetSelection` で1回の Undo。範囲は選択の上限 4096 タイル（`crates/alto-engine/src/selection.rs` の `MAX_SELECTION_TILES`）。
- `replacePixels`（新規）：`ImageEdit`（`crates/alto-engine/src/image/mod.rs`）に新しい種類を足す。`ImageBounds` の範囲で検証し、選択があればその範囲だけ置き換える。結果の型は既存の `ImageEditResult`。
- データは transfer で WASM へ1回コピーする。

## SEL-4 被写体選択（除外）

**設計。** `examples/` にブラウザ内推論（onnxruntime-web など）の例を置く案。`exportRegionPixels` で入力を得て、マスクを `fromPixels` へ渡す（新規）。エンジンに依存を足さない。

## AI-3 生成塗りつぶし（除外）

**設計。** 同じ形。組み込む側が `exportRegionPixels` と選択のアウトラインを生成サービスへ送り、結果を新しいラスターレイヤーで `replacePixels` する（新規）。外部への送信は組み込む側の責任と同意の範囲で、SDK は行わない。

編集の意図は、組み込む側の識別子 `app:generated`（`packages/alto-sdk/src/intents.ts` の `IntentId` は `app:` で始まる識別子を許す）と `doc.transact` のラベルで記録できることを文書で示す。

## COLLAB-1 共同編集

### 現状

編集の経路（すべて main に在る）：

- `crates/alto-doc/src/editor.rs`：`Editor::transact(intent, ops, base_revision)` は `base_revision` が `doc.revision` と違えば `DocError::Conflict`（`CONFLICT`）。成功すると `AppliedOp { op, inverse }` の列を `Transaction` に入れて undo 積みへ積み、redo を消す。`undo` / `redo` は積みの先頭の `inverse` / `op` を `replay` する。`max_undo` は 100。`IntentLogEntry` は版の commit の意図の一覧。
- `crates/alto-doc/src/op.rs`：`Op` は 16 種（`InsertNode`、`RemoveNode`、`RestoreSubtree`、`MoveNode`、`SetProps`、`SetMask`、`SetAdjustment`、`SetTiles`、`SetVector`、`SetVectorMask`、`SetClipPath`、`SetText`、`SetSelection`、`SetAsset`、`SetMeta`、`ReplaceNode`）。`SetProps` は `PropPatch`（フィールドごとの `Option`）。`SetTiles` は `TileChange { key, before, after }`。
- `crates/alto-doc/src/document.rs`：`SetTiles` は各 `before` が今のタイルと違えば `INVALID_ARGUMENT`（`Tile precondition failed`）。その逆 Op は**ノード全体**の `ReplaceNode`（旧ノード）。`SetMask`、`SetVector`、`SetText` などは値全体を入れ替え、逆 Op は旧い値全体。`enforce_lock` は `locked` のノードへの編集を拒む。
- `NodeId` は UUIDv7（`crates/alto-core/src/lib.rs`）。SDK の `Transaction`（`packages/alto-sdk/src/transaction.ts`）も UUIDv7 を作る。2つの端末が同時に作っても衝突しない。
- `crates/alto-doc/src/digest.rs`：`StateDigest` は文書の内容を表すハッシュ。編集ごとに増分で更新する。**選択（`selection`）を含む**。
- `crates/alto-engine/src/lib.rs`：`Engine::transact` は、参照するオブジェクトの在否、マスク面、ノードの内容、`SetTiles` のタイル形式を検証してから `Editor::transact` を呼ぶ。`install_object(hash, encoded)` は符号化されたオブジェクトを検証して入れる（`pub`）。`encoded`（オブジェクトを符号化して返す）は private。`begin_tile_stage`、`stage_tile`、`release_staged_tiles` は、取引が終わるまでタイルを GC から守る。
- ジャーナル：`Engine::journal_record(h, kind, intent, ops)` が `transaction.json`（`kind`、`intent`、`ops`、`after`）と、ops が参照するオブジェクトを 1 つの記録にする。`Engine::replay_record` が再生し、`after` と合わなければ戻して `Replayed::Diverged`。`JournalKind` は `Transact`、`Undo`、`Redo`。
- `crates/alto-wasm/src/lib.rs`：利用者の編集は `Runtime::transact`、`undo`、`redo`、`end_brush`（ストロークの確定。`SetTiles`、マスクでは `SetMask` を作る）、`command`（選択、マスク、効果）で `Engine::transact` を通り、`journal.push` で記録する。画像編集は `crates/alto-wasm/src/image_versions.rs`、版の操作は `crates/alto-wasm/src/versions.rs`。ストローク中は他の編集を `CONFLICT: finish active tool first` で拒む。
- `crates/alto-engine/src/text.rs`：`bake_text_ops` は、`bake` を持たない `SetText` を焼き込む。**持つ Op はそのまま**（「焼き込みは他者の Op から再計算しない」）。
- SDK：`DocumentHandle.transact`（`packages/alto-sdk/src/document.ts`）は `baseRevision` を付けて `transact` を要求する。`edit()` は先に進行中のストロークを終わらせる。要求の型は `packages/alto-sdk/src/protocol.ts`（`PROTOCOL = 8`）。エラーは `packages/alto-sdk/src/errors.ts` の `ErrorCode` と `ConflictReason`。SDK に WebSocket などの通信は無い（README の「外部サーバーへ送信する処理はありません」）。
- 版を `.alto` の外に持つ（F-9、[設計](../2026-11-16-versions-outside-alto.md)）：`save(target, { history: "omit" })` は作業文書だけの `.alto`。`save(target, { history: { folder } })` は薄い `.alto`。`engine.open(source, { history })` で置き場をつなぐ。
- 無いもの：リモートの Op を適用する入口、Op の作者（actor）、順序の番号、組み込み側へ編集を渡す取り付け口、通信のパッケージ。

### 11-04 からの変化

- `Editor`、`Op`、`Document::apply` は 126f2b5 で `lib.rs` から `editor.rs`、`op.rs`、`document.rs` に分かれた。中身は変わっていない。11-04 版の「`crates/alto-doc/src/lib.rs` を変更」は `editor.rs` と `op.rs` になる。
- 11-04 版が前提にした「`SetTiles` の `before` の照合で Undo が失敗する」は誤りだった。`SetTiles` の逆 Op は `before` を持たない `ReplaceNode`（ノード全体）なので、他者の後でも**黙って成功し、他者のタイルや属性を巻き戻す**。Undo は積みの `inverse` を使わず作り直す（設計の「Undo」）。
- 堅牢化 A〜F で、ジャーナルは増分の版の記録（F-4）を持ち、復旧が `lazy-objects.json` を持つ（F-9）。共同編集の記録もこの枠に足す。
- 版が `.alto` の外に出た（F-9）。共同編集で共有するのは作業文書だけとし、版は各参加者が持つ。
- `ErrorCode` と `ConflictReason` は 11-04 から変化がほぼ無い（`errors.ts` は PNG の書き出しの修正のみ）。
- 11-04 版の経路 `m5/mod.rs` は `crates/alto-gpu/src/renderer/` に分かれた。共同編集は描画に触れない。

### 設計

**方針。** 中央は順序を付けるだけにし、**適用は全員が同じ決定的な関数で行う**。全員が同じ初期状態から同じ順序の取引を同じ関数で適用すれば、同じ状態になる。サーバーは Op の意味を知らない。CRDT と ECS は採らない（設計書の決定）。

#### 取引の流れ

1. 参加者の編集は今までどおり楽観的に適用する。取引ごとに `CollabTx`（新規）を作って**未確定（pending）**の列に積み、通信層が順序係へ送る。
2. 順序係は届いた順に番号 `seq`（1 から）を付けて全員へ配る（送り主へも。これが確定の通知）。
3. 他者の取引 R を受けたら、未確定をすべて巻き戻し、R を適用し、未確定を作り直す（rebase）。自分の取引の通知は、`id` が一致する未確定を外すだけ。1つの接続の `submit` は到着順に番号が付くので、自分の取引は送った順に確定する。先頭と `id` が違えば `CORRUPT_DATA` として基点から取り直す。状態はすでに一致している（作り直しのたびに確定済みの状態から計算し直すため）。
4. 受け取る側は `seq` の順にしか適用しない。ストロークの最中（`sampler` あり）に届いた取引は、SDK が溜めて、ストロークの確定の後に適用する。

```rust
// crates/alto-doc/src/collab.rs（新規）
pub struct CollabTx {
    pub id: String,                      // "<client>:<連番>"。再送の重複排除に使う
    pub intent: String,
    pub label: Option<String>,
    pub ops: Vec<Op>,                    // SetSelection と SetMeta を含まない
    pub bases: BTreeMap<usize, OpBase>,  // ops の添字 → 送り手が編集した元の値
}
pub enum OpBase {
    Text(Box<TextContent>),              // SetText の編集前の内容（逆 Op から取る）
    Mask(Option<TileMap>),               // SetMask の編集前のタイル表
}
pub enum Applied { Applied(ChangeSet), Void, Rejected(DocError) }  // Void は全 Op が対象を失った
```

#### 決定的な変換 `plan`

`Editor::collab_plan(&self, tx: &CollabTx, merge_text: &dyn Fn(&TextContent, &TextContent, &TextContent) -> Option<TextContent>) -> Vec<Op>`（新規）は、取引の直前の文書だけから Op の列を作る純粋な関数。Op ごとに次の規則で変える。

| Op | 規則 |
|---|---|
| `InsertNode`、`RestoreSubtree` | 親が無い、または id が在れば捨てる（削除が勝つ）。`index` は親の子の数へ切り詰める（同じ取引で作った親は切り詰めない） |
| `RemoveNode` | 対象が無ければ捨てる（冪等） |
| `MoveNode` | 対象か移動先が無ければ捨てる。`index` は切り詰める。循環になるなら取引全体が `Rejected` |
| `SetProps` | 対象が無ければ捨てる。フィールドごとに後勝ち（`PropPatch` の `Some` だけが効く） |
| `SetTiles` | 対象が無い、またはラスターでなければ捨てる。各 `before` を今のタイルに書き換える（タイルごとの後勝ち）。書き換えた後 `before == after` の項目は捨てる |
| `SetMask` | `bases` の `Mask` を元に、変えられたタイルだけを今のタイル表へ重ねる（タイルごとの後勝ち）。`Mask` が無ければ値全体の後勝ち |
| `SetText` | 今の内容が `bases` の `Text` と同じなら Op の内容。違えば `merge_text(base, 今, Op)`。統合できなければ Op の内容（後勝ち）。`bake` は外す |
| `SetVector`、`SetVectorMask`、`SetClipPath`、`SetAdjustment`、`ReplaceNode`、`SetAsset` | 値全体の後勝ち。対象が無ければ捨てる |
| ロックされた対象 | 変換しない。適用が `enforce_lock` で失敗し、取引全体が `Rejected`（ロックは文書の状態なので全員が同じ結果になる） |
| `SetSelection`、`SetMeta` | 送らない。受けたら取引全体が `Rejected` |

`SetText` の `bake`（描画の版 3）は、変換の後に `Engine::bake_text_ops` が全員の側で焼き直す。統合で内容が変わるので、送り手の焼き込みは使えない。（新規）そのため接続の条件に、同じエンジンのビルド（`wasm` のハッシュ）を要る（下の「接続」）。焼き込みは決定的で、食い違いは収束の検査（後述）が見つける。`merge_text` は VER-3 が作る文字範囲の 3 者統合（08 の `merge_text`）を、エンジンから渡す。`alto-doc` は統合の実装を持たない。

適用は `Editor::collab_apply_planned(ops) -> Result<(Vec<AppliedOp>, ChangeSet), DocError>`（新規）。既存の `replay`（`enforce` あり、失敗は巻き戻して状態を変えない）の後に `revision` を 1 上げ、`StateDigest` を更新する。**undo / redo の積みにも、意図の一覧（`IntentLogEntry`）にも入れない。** 他者の編集は自分の Undo の対象にならない。

#### 衝突の解消

| 起きること | 結果 |
|---|---|
| 同じプロパティを同時に変えた | 順序が後の取引が勝つ（フィールドごと） |
| 同じタイルを同時に描いた | 後の取引のタイルが残る。ストロークの描画は読み戻した結果なので、そのまま置ける |
| 別のタイルを同時に描いた | 両方残る（`SetTiles` はタイルごとに変換する） |
| 片方がノードを削除し、片方が編集した | 削除が勝つ。編集の Op は捨てる。取引のすべてが対象を失えば `Void`（`onDropped`（新規）の通知） |
| 同じ文字を同時に編集した | 範囲が重ならなければ両方（`merge_text`（新規））。重なれば後の取引 |
| 同じ位置へ同時に挿入・移動した | 順序の通りに適用され、`index` は切り詰める |

自分の未確定の取引が、他者の取引の後で適用できなくなったとき（ロック、削除されたノード）は、**捨てずに休止（suspended）**にする。その後の作り直しでまた試す。他者の取引が復元（Undo による `RestoreSubtree`）で対象を戻すことがあり、送り手が早く捨てると他の参加者との結果が割れるため。通知が来た時点で適用できなければ、全員が同じく `Rejected` / `Void` とみなし、`onDropped(tx, reason)`（新規）で組み込み側に返す（ストロークのタイルを別の層へ貼る余地を残す）。

#### Undo と Redo

他者の取引は自分の undo / redo の積みに入らないので、`actor` による絞り込みは要らない。次の 2 点だけ変える（共同編集が有効なときだけ）。

- Undo は積みの `inverse` を使わない。`AppliedOp.op` と今の文書から、**守り付きの逆 Op**を作り、新しい取引（`edit.undo`）として未確定に積んで配る。守りは「今の値が、その Op が置いた値のままのときだけ戻す」。
  - `SetTiles`：項目ごとに、今のタイルが `after` と同じなら `before` へ戻す。違う項目（他者が上書きした）は戻さない。
  - `SetProps`：フィールドごとに同様。
  - `SetText`：`merge_text(base = Op の内容, 今, 逆 Op の旧い内容)`。統合できなければ戻さない。
  - 値全体の Op（`SetVector`、`SetVectorMask`、`SetClipPath`、`SetAdjustment`、`ReplaceNode`）：今の値が Op の値と同じときだけ戻す。
  - `MoveNode`：ノードと元の親が在り、今の親と位置が Op の移動先のままのときだけ元へ戻す（`index` は切り詰める。循環になるなら戻さない）。
  - `SetAsset`：今の記録が Op の記録と同じときだけ旧い記録へ戻す。
  - `InsertNode` の逆（削除）：ノードが在り、他者が子を足していなければ。`RemoveNode` の逆（`RestoreSubtree`）：親が在り、復元するノードの id が空いていれば（`index` は切り詰める）。
  - 戻せる Op が 1 つも無ければ、積みの項目を外し、`onUndoSkipped({ applied: 0, skipped: n })` を出す。一部だけ戻せたときも同じ通知（`applied` と `skipped`）。
- Redo は元の `op` をもう一度 `plan` に通した取引。

休止中の取引の項目は Undo できない（`CONFLICT`、理由 `collab-pending-rejected`（新規））。

#### 接続中に使えない操作

第一段階では、共有する作業文書を丸ごと入れ替える操作を断る。`CONFLICT`（理由 `collab-active`（新規））。

- `restoreVersion`、ブランチの切り替え（`checkout`）、マージの確定（`finish_merge`）、stash の取り出し（VER-4）、`engine.open` による文書の差し替え。
- キャンバスの寸法を変える編集（`SetMeta`、`image.crop`、`image.resize` など `canvas.rs` が `SetMeta` を出すもの）。

commit、ブランチの作成、タグ、`pruneVersions`、保存は使える。commit は共有の作業文書の今の状態を記録する（意図の一覧は自分の編集だけ）。

#### 選択は共有しない

`SetSelection` は参加者ごとの状態。送らず、受けない。収束の確認には `Editor::shared_digest()`（新規）。`StateDigest` の `combine` から `selection` を除いた値を使う。

#### オブジェクト（タイル、画像、フォント）

取引の `ops` が参照するハッシュは `crates/alto-engine/src/lib.rs` の `op_hashes`（private）と同じ集合（`submit` の `objects`、送信時に計算）で一覧にし、**順序のチャネルとは別のチャネル**で運ぶ（内容アドレス。VER-6 と同じ符号化 `alto_store::encode_object`）。

- 送り手は、取引を送る**前**にオブジェクトを置き場へ置き、完了してから `submit` する。順序係は `seq` を付ける処理でオブジェクトを待たない。
- 受け手は、`Engine::collab_missing(h, &tx) -> Vec<ContentHash>`（新規）で足りないものを知り、取得して `install_object` し（ハッシュの検証を含む）、**適用の直前まで** `stage_tile` で守る。取得を待つ間、後続の取引の適用は止まる（順序を守るため）。`seq` の付与と通知は止まらない。
- 大きなタイルが編集の順序を遅らせないことは、2 つのチャネルが別の接続であることと、取得の時間切れ（下の上限）で守る。

#### 接続と通信（`packages/alto-collab/`（新規））

- 順序のチャネル（WebSocket、JSON のテキストフレーム）：
  - 参加者 → 順序係：`hello { protocol, room, client, documentFormat, renderVersion, engine, mode: "edit" | "view", resume? }`、`submit { id, baseSeq, intent, label?, ops, bases?, objects }`、`digest { seq, hash }`。
  - 順序係 → 参加者：`welcome { baseHash, baseBytes, headSeq, limits }`、`tx { seq, client, id, ... }`（送り主にも）、`reject { id, code }`、`diverged { seq }`。`client` は順序係が接続ごとに付ける。参加者の自己申告は使わない。
- オブジェクトのチャネル（別の WebSocket、バイナリ）：`put`、`get`、`data`、`missing`。置き場は順序係の実装の中にあり、検証はしない。受け手が必ず検証する。
- 参加の始め：開く側 `host(doc, { url, room })` は `doc.save(target, { history: "omit" })` が書く作業文書だけの `.alto`（版を含まない）を基点として置く。参加する側 `join({ url, room })` は基点を取って `engine.open` し、`seq` 1 から最新までを適用する。参加者は `documentFormat`（01 が入れる `alto_doc::FORMAT_VERSION`（新規）の値）、描画の版、エンジンのビルドが基点と同じでなければ `hello` で断られる（`FORMAT_MISMATCH`）。
- 切断：未確定の取引はそのまま残り、文書は触れる。再接続で `hello { resume: 最後に適用した seq }` を送り、取りこぼしを受けて rebase し、未確定を順に再送する。`id` が同じ取引は順序係が重複とみなし、前の `seq` を返す。切断中のストロークも同じ（ストロークの確定後、未確定に積まれ、再接続の後に順序が付く）。
- 収束の確認：未確定が空のときに `seq` が 64 の倍数を越えたら、`shared_digest`（新規）を `digest` で送る。順序係は同じ `seq` の値が割れたら `diverged` を配る。受けた参加者は基点から取り直す（`rejoin`。未確定は `onDropped` に返す）。
- 運用向けのサーバー、認証、権限、`view` の強制、ログの圧縮は本リポジトリの範囲外。`examples/collab-relay/`（新規）は順序付けだけをする参照実装（Node の WebSocket、認証なし、ローカル確認用）。

#### SDK の取り付け口（通信を持たない）

`@alto/engine/collab`（新規の subpath）。`DocumentHandle` に副作用を足さず、取り付けた時だけ動く。

```ts
export function attachCollab(doc: DocumentHandle, options: { actor: string }): CollabPort; // 新規
export interface CollabPort {
  /** 自分の取引。適用された順。再送のため、確定されるまで残る。 */
  onOutgoing(listener: (tx: OutgoingTx) => void): () => void;
  /** 確定した取引（自分のものも）を seq の順に渡す。 */
  confirm(entry: SequencedTx): Promise<ConfirmResult>;
  missingObjects(entry: SequencedTx): Promise<string[]>;
  readObject(hash: string): Promise<Uint8Array | null>;
  installObject(hash: string, bytes: Uint8Array): Promise<void>;
  digest(): Promise<{ seq: number; pending: number; hash: string }>;
  onDropped(listener: (tx: OutgoingTx, reason: "void" | "rejected") => void): () => void;
  onUndoSkipped(listener: (r: { applied: number; skipped: number }) => void): () => void;
  detach(): Promise<void>;
}
```

`attachCollab`（新規）の後、`doc.transact`、ストローク、`undo` / `redo`、`select` 系の編集は今までの API のまま。ストロークの最中に届いた `confirm` は SDK が溜め、`toolEnded` の後で処理する。`confirm` は `edit()` を通らず（進行中のストロークを終わらせないため）、`core.mutate` の中で行う。

#### 上限とエラー

| 定数 | 値 | 超えたら |
|---|---|---|
| `COLLAB_MAX_OPS_PER_TX`（新規） | 1024 | 送る側は `RESOURCE_LIMIT`、受ける側は `reject`（`TX_TOO_LARGE`） |
| `COLLAB_MAX_TILES_PER_TX`（新規） | 4096（選択の上限と同じ） | 同上 |
| `COLLAB_MAX_TX_BYTES`（新規） | 8 MiB（ops の JSON） | 同上 |
| `COLLAB_MAX_OBJECT_BYTES`（新規） | 64 MiB（符号化後） | 置かない、取得もしない |
| `COLLAB_MAX_PENDING`（新規） | 256 | 新しい編集を `RESOURCE_LIMIT`（理由 `collab-pending`（新規））で断る。文書は変わらない |
| `COLLAB_OBJECT_TIMEOUT_MS`（新規） | 30000 | 取得を諦め、`diverged` の経路で基点から取り直す |
| `COLLAB_MAX_CLIENTS`（新規） | 8 | `hello` を断る |
| `COLLAB_MAX_LOG_TXS`（新規） | 20000 | 部屋を読み取り専用にし、新しい部屋への引き継ぎを促す |

受け側は**すべての取引を、自分の側で検証する**。`Engine::transact` の検証（参照オブジェクトの在否、マスク面、ノードの内容、タイル形式）を `Engine::check_ops(h, &ops)`（新規）に取り出し、`transact` と共同編集の受信が同じ関数を使う。加えて取引の大きさ、Op の数、`NodeId` の重複、`InsertNode` で入ってくる木の深さ（`MAX_TREE_DEPTH`）を、適用前に数える。古い、または悪意のあるクライアントの取引は、`Rejected` として全員が同じく捨てる。エラーは既存の `ErrorCode` を使い、共同編集の理由は `ErrorDetails.reason`（`collab-active`、`collab-pending`、`collab-pending-rejected`。いずれも新規）で区別する。通信の失敗（`FORMAT_MISMATCH`、`TX_TOO_LARGE` など）は `@alto/collab` の `CollabError` にする。

#### 利用者の決定が要る点

| 問い | 案（既定を先頭に） |
|---|---|
| 共有するのは作業文書だけでよいか（版は各自） | 作業文書だけ。版も共有するなら VER-6 の fetch / push と組み合わせる |
| 接続中のキャンバス寸法の変更 | 断る。許すなら `SetMeta` を後勝ちにして、全レイヤーの `transform` の競合を解く規則が要る |
| 順序係の認証と権限 | 範囲外（参照実装は認証なし）。組み込み側が用意する |
| ログの圧縮（新しい基点の作成） | 第一段階は無し（`COLLAB_MAX_LOG_TXS`（新規）で部屋を閉じる） |
| 他者の編集を commit の意図の一覧に入れるか | 入れない（`IntentLogEntry` に作者のフィールドを足すと版の形式に触れる） |

### 形式と通信

- `format_version`：**上げない**。文書に新しいフィールドを足さない。共同編集の通信が運ぶ `Op` の JSON は文書の形式に結び付くので、`hello` で `FORMAT_VERSION`（新規）（01 の RV4-1 が作る）（いまは `DocumentMeta.format_version` が 5 固定）の一致を要る。形式が上がった後は、旧い形式の参加者を断る。
- manifest：変えない。`document_format`、`.altoproj`、薄い `.alto`、`LazyObjects` は触れない。基点の `.alto` は既存の `save(target, { history: "omit" })`。
- 描画の版：触れない。共同編集は描画に触れない。`render_version` は `hello` で一致を見る。v4 が入るのは RV4-1 の後で、その後は v3 と v4 の文書を別の部屋として扱う。
- `PROTOCOL`：**上げる**（値は要割り当て）。要求 `collabAttach`、`collabConfirm`、`collabMissing`、`collabReadObject`、`collabInstallObject`、`collabDigest`、`collabDetach`（新規）と、ワーカーから出る `outgoing` の通知を足す。README と RELEASING を同じ PR で更新する。
- 通信のプロトコル：`@alto/collab` の `COLLAB_PROTOCOL`（新規）の初版の値。SDK の `PROTOCOL` とは別に持つ。
- 復旧のジャーナル：共同編集の確定を表す記録 `JournalKind::Collab`（新規）を足す。`transaction.json` に `entry`（`seq`、`client`、`id`、`ops`、`bases`）と、自分の取引の通知かどうか（`own`）を入れる。`journal_record` はオブジェクトも入れる。チェックポイントに `collab.json`（新規。`actor`、確定済みの `seq`、未確定の列、休止中の印）を足す。復旧は同じ関数で再生し、`after` が合わなければ `Replayed::Diverged` を返す（既存の扱い）。復旧後は `resume` で再接続できる。旧い記録を読む必要はない（共通規則3、移行なし）。
- `ErrorCode`：足さない。`ConflictReason` に `collab-active`、`collab-pending-rejected`、`ErrorDetails.reason` に `collab-pending` を足す（`packages/alto-sdk/src/errors.ts`）。
- ネットワーク処理は SDK 本体に入れない。`packages/alto-sdk/package.json` の依存と `exports` には通信を足さない（`./collab` は取り付け口だけ）。

### 作業手順

三段に分ける。1a で収束を確かめ、収束しなければ本書を更新してから 1b に進む。

#### Task COLLAB-1a：収束のシミュレーション（`alto-doc` だけ、エンジンなし）

**Files:** Create `crates/alto-doc/src/collab.rs`（新規。`CollabTx`、`OpBase`、`plan`）、`crates/alto-doc/tests/collab_sim.rs`（新規）。Modify `crates/alto-doc/src/editor.rs`（`collab_apply_planned`、取り消し可能な rebase の部品）、`crates/alto-doc/src/lib.rs`（`mod collab` と公開）。

**Interfaces:** Produces `Editor::collab_plan`、`collab_apply_planned`、`shared_digest`。タイルのハッシュは試験では任意の値（`Editor` はタイルの実在を見ない）。

- [ ] 試験 `two_clients_converge_for_any_interleaving`（proptest）：2〜3 の `Editor` が、プロパティの変更、追加、削除、移動、タイル、テキスト（`merge_text` の代わりに試験用の統合）を任意の順で作り、順序係の役の関数が番号を付けて配る。全員の `shared_digest` が一致する。
- [ ] 試験 `a_rejected_remote_op_leaves_a_valid_document`（proptest）：他者の削除で対象を失った Op を捨てても `validate_graph` が通り、`Rejected` の取引が状態を変えない。
- [ ] 試験 `pending_ops_are_suspended_not_dropped`：削除と復元が続いて届くとき、休止中の取引が復元の後に適用され、全員の結果が一致する。
- [ ] 試験 `tile_edits_to_different_tiles_both_survive`、`same_tile_last_writer_wins`、`delete_beats_edit`、`locked_target_rejects_the_whole_tx`。収束だけでなく、期待するタイル表・ノードの有無・プロパティの値を直接比べる（同じ誤りへの収束を見逃さない）。proptest には、順序どおり1台で適用した結果との一致も加える。
- [ ] 収束しない例を見つけたら最小の例を本書の設計に記録し、規則を直してから進む。
- 完了条件：上の試験が通る（proptest の既定の件数と、`PROPTEST_CASES=2000` の 1 回）。
- 検証：`cargo test -p alto-doc`、`sh scripts/check.sh native`。

#### Task COLLAB-1b：エンジンと SDK の取り付け口

**Files:** Create `crates/alto-engine/src/collab.rs`（新規。`Engine::collab_*`）。Modify `crates/alto-engine/src/lib.rs`（`check_ops` の取り出し、`export_object`（新規。`encoded` の公開）、`JournalKind::Collab`、`replay_record`）、`crates/alto-engine/src/text.rs`（`bake_text_ops` を変換後の Op に使う）、`crates/alto-engine/src/versions/merge.rs`（VER-3 の `merge_text` を呼べるようにする）、`crates/alto-engine/src/versions.rs` と `crates/alto-engine/src/versions/branches.rs`（接続中の拒否）、`crates/alto-wasm/src/lib.rs`、`crates/alto-wasm/src/image_versions.rs`、`crates/alto-wasm/src/versions.rs`（編集の出口で外向きの取引を積む）、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/errors.ts`、`packages/alto-sdk/src/collab.ts`（新規）、`packages/alto-sdk/package.json`（`exports` に `./collab`）、`packages/alto-sdk/api/alto-engine.api.md`、`README.md`、`RELEASING.md`、`doc/MILESTONES.md`（上限の節）。

**Interfaces:** Consumes 1a の `collab_plan` と `collab_apply_planned`。Produces `Engine::collab_attach`、`collab_apply`、`collab_missing`、`collab_take_outgoing`、`collab_digest`、`attachCollab`、`CollabPort`。

- [ ] 試験（Rust）`undo_only_touches_the_local_actors_transactions`：他者の取引が自分の undo 積みに入らず、Undo が他者の変更を戻さない。
- [ ] 試験（Rust）`undo_skips_tiles_a_peer_overwrote`：他者が上書きしたタイルは戻らず、他のタイルは戻る。`undo_blocked_when_nothing_can_be_reverted`。
- [ ] 試験（Rust）`undo_text_merges_around_a_peers_edit`。
- [ ] 試験（Rust）`rebase_of_pending_local_ops_is_invisible_after_the_final_order`：他者の取引が割り込んだ後の確定で、自分の文書の `shared_digest` が一致する。
- [ ] 試験（Rust）`remote_text_is_rebaked_locally`：描画の版 3 で、統合後の `SetText` が全員同じ `bake` になる。
- [ ] 試験（Rust）`journal_replay_restores_remote_and_local_order`：local、remote、own の記録を再生して `after` が一致し、不一致なら戻して `Diverged`。
- [ ] 試験（Rust）`remote_tx_with_unknown_object_waits_and_applies_in_order`、`remote_tx_over_the_limits_is_rejected_atomically`、`restore_and_checkout_are_refused_while_attached`。
- [ ] 試験（SDK）`attachCollab does not end an active stroke`、`confirm during a stroke is applied after it ends`。
- [ ] 試験（SDK）`@alto/engine exports no network`：`dist` のどの入口にも `WebSocket`、`fetch`、`XMLHttpRequest` の呼び出しが無い。
- [ ] 実装、PR。
- 完了条件：上の試験、既存の全試験、API の変更の記録。
- 検証：`cargo test -p alto-engine`、`cargo test -p alto-doc`、`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`pnpm --dir packages/alto-sdk api:check`、`node scripts/verify-browser.mjs --functional`。

#### Task COLLAB-1c：通信と参照の順序係

**Files:** Create `packages/alto-collab/package.json`、`packages/alto-collab/src/index.ts`、`packages/alto-collab/src/session.ts`、`packages/alto-collab/src/wire.ts`、`packages/alto-collab/test/session.test.js`（いずれも新規）、`examples/collab-relay/server.mjs`（新規）、`examples/collab-relay/README.md`（新規）、`tests/e2e/collab-tests.js`（新規）。Modify `scripts/verify-browser.mjs`（`--suite=collab`）。

**Interfaces:** Consumes `attachCollab`、`CollabPort`（1b）。Produces `host`、`join`、`CollabError`、`COLLAB_PROTOCOL`。

- [ ] 試験（Node、模擬の通信）`submit waits for objects then orders`、`a reconnect resumes and resends pending once`、`a duplicate id gets the first seq`、`a slow object does not delay seq assignment`。
- [ ] ブラウザ試験（2 タブ）`two tabs converge after concurrent edits`、`a reconnect replays missed transactions`、`objects transfer by hash`、`a stroke on a layer deleted by a peer is reported as dropped`、`a stroke made while disconnected is ordered after reconnect`。
- [ ] 試験 `hello with another documentFormat is refused`、`a diverged digest makes every client rejoin`。
- [ ] 実装、PR。
- 完了条件：上の試験。`examples/collab-relay` だけを外した状態で SDK 本体が通信を持たないこと。
- 検証：`node --test packages/alto-collab/test`、`node scripts/verify-browser.mjs --suite=collab`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に：VER-3（`merge_text`（新規））、VER-6（オブジェクトの符号化と内容アドレスの転送の形を揃える。コードの依存は無いが、同じ符号化を使う）。01 の RV4-1（`FORMAT_VERSION`（新規）の一致を `hello` で見る。無ければ現在の 5 を直接比べ、RV4-1 の後で置き換える）。
- 後に：なし。

## Review Focus

- 切断中のストロークが再接続後に確定する順序。
- 他者の削除したレイヤーへ描いている途中のストローク。
- 悪意のある、または古いクライアントの Op の検証。受信側でも全 Op を検証する。
- 大きなタイルの転送が編集の順序を遅らせないこと。オブジェクトと順序を別のチャネルにする。
- 共同編集中のコミットとマージの扱い。第一段階では、接続中のブランチ操作を拒否する。
- 休止中の取引と、復元で戻る対象の組（早く捨てると参加者ごとに結果が割れる）。
- `SetTiles` の逆 Op が `ReplaceNode`（ノード全体）であること。Undo に積みの `inverse` を使うと他者の編集を戻す。
- `PROTOCOL` を上げる PR が、同じ時期の他の項目の上げと同じ番号にまとまっているか。
