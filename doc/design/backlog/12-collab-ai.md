# 共同編集と AI 機能

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 共同編集を実装する。AI 機能は除外の記録として残す。

**Architecture:** README のとおり、画像や編集内容を外部へ送る処理は SDK 本体にない。共同編集の通信は別パッケージに置く。

状態：COLLAB-1 は実装の対象。AI-2、SEL-4、AI-3 は除外（2026-11-04、利用者の決定）。

## AI-2 外部画素とマスクの取り込み API（除外）

**設計。** 被写体選択と生成塗りつぶしの共通の土台。組み込む側がモデルを呼び、結果をエンジンへ渡す。

```ts
doc.selection.fromPixels({ x, y, width, height, data: Uint8Array /* R8 */, mode? }): Promise<void>
doc.image.replacePixels(node, { x, y, width, height, data: Uint8ClampedArray /* RGBA8 */, selection?: "use" | "ignore" }): Promise<ImageEditResult>
doc.exportRegionPixels({ region, source: "layer" | "composite", node? }): Promise<ImageData>
```

- `fromPixels`：`SetSelection` で1回の Undo。範囲は `MAX_SELECTION_TILES`。
- `replacePixels`：既存の `ImageEdit` の新しい種類。`ImageBounds` の範囲で検証し、選択があればその範囲だけ置き換える。
- データは transfer で WASM へ1回コピーする。

## SEL-4 被写体選択（除外）

**設計。** `examples/` にブラウザ内推論（onnxruntime-web など）の例を置く案。`exportRegionPixels` で入力を得て、マスクを `fromPixels` へ渡す。エンジンに依存を足さない。

## AI-3 生成塗りつぶし（除外）

**設計。** 同じ形。組み込む側が `exportRegionPixels` と選択のアウトラインを生成サービスへ送り、結果を新しいラスターレイヤーで `replacePixels` する。外部への送信は組み込む側の責任と同意の範囲で、SDK は行わない。

Intent に `generated` を出せるよう `doc.transact` のラベルで記録できることを文書で示す。

## COLLAB-1 共同編集

**設計。** 設計書は CRDT と ECS を採らないとし、ID と Op の設計で将来の余地を残した。最も単純な案は中央の順序付けである。

- **モデル。** 中央サーバーが Transaction に全順序を与える。クライアントは楽観的に適用し、順序が確定したら巻き戻して再適用する（`baseRevision` と `CONFLICT` の既存の形）。
- **衝突の解消。** プロパティは後勝ち、ノードの削除が編集に勝つ、タイルは後勝ち（版のマージと同じ単位の競合は作らない）。テキストは VER-3 の差分で変換する。
- **ストローク。** 確定まで他者に見えない。確定後は `SetTiles` が他者のタイルを上書きする可能性がある。読み戻した結果なので再適用できる。
- **Undo。** 他者の編集の後に戻すと、タイルの `before` の照合に失敗する。自分の Transaction の Undo を、失敗したタイルについては拒否する。
- **オブジェクト。** 内容アドレスで転送する（VER-6 と同じ形）。
- **エンジンの変更。** `doc.applyRemote(transaction)`、Undo の `actor` フィルタ、ジャーナルの `remote` 区別。
- **サーバー。** 順序付けだけを行う参照実装を `examples/collab-relay` に置く（Node の WebSocket、認証なし、ローカル確認用）。運用向けのサーバーは本リポジトリの範囲外。通信は別パッケージ `packages/alto-collab` で、SDK 本体に WebSocket を入れない。

未解決：Undo の見た目の一貫性と、文書の検証エラー（他者の削除で無効になる Op）の扱い。この2点は試作で確かめないと確定できない。最初の作業としてメモリ内の2クライアントのシミュレーションを `alto-doc` の試験で書き、収束の性質を proptest で確かめる。収束しなければ本書を更新してから本体に着手する。

## 実装タスク

### Task COLLAB-1a: 収束のシミュレーション

**Files:** Create `crates/alto-doc/tests/collab_sim.rs`。

- [ ] proptest `two_clients_converge_for_any_interleaving`：プロパティの変更、追加、削除、移動、タイル。
- [ ] proptest `a_rejected_remote_op_leaves_a_valid_document`：他者の削除で無効になる Op を破棄しても文書の検証が通る。
- [ ] 収束しないケースを記録し、本書の設計を更新する。

### Task COLLAB-1b: エンジンの変更

**Files:** Modify `crates/alto-doc/src/lib.rs`、`crates/alto-engine/src/lib.rs`、`crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/document.ts`、`protocol.ts`。

**Interfaces:** `doc.applyRemote(transaction, { order })`、`doc.onLocalTransaction(listener)`、`Transaction.actor`。

- [ ] 試験 `undo_only_touches_the_local_actors_transactions`、`undo_is_refused_for_tiles_a_peer_overwrote`。
- [ ] 試験 `rebase_of_pending_local_ops_is_invisible_after_the_final_order`。
- [ ] 試験 `journal_replay_restores_remote_and_local_order`。
- [ ] 実装、PR。

### Task COLLAB-1c: 通信と参照サーバー

**Files:** Create `packages/alto-collab/`、`examples/collab-relay/`。

**Interfaces:** Consumes `applyRemote`、`onLocalTransaction`。

- [ ] ブラウザ試験（2タブ）`two tabs converge after concurrent edits`、`a reconnect replays missed transactions`、`objects transfer by hash`。
- [ ] `@alto/engine` に WebSocket が入らないことを試験。
- [ ] 実装、PR。

## Review Focus

- 切断中のストロークが再接続後に確定する順序。
- 他者の削除したレイヤーへ描いている途中のストローク。
- 悪意のある、または古いクライアントの Op の検証。受信側でも全 Op を検証する。
- 大きなタイルの転送が編集の順序を遅らせないこと。オブジェクトと順序を別のチャネルにする。
- 共同編集中のコミットとマージの扱い。第一段階では、接続中のブランチ操作を拒否する。
