# 組み込み用 API の実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** SDK を、組み込む側が型と約束に頼れる API（`@alto/engine` ほか 4 つの入口）に作り直す。

**Architecture:** エンジン（Rust / WASM / Worker）の描画と保存はそのままにする。表示用の文言を識別子と警告の種類に置き換え、SDK を役割ごとのモジュールに分ける。DOM、大きさ、保存場所の名前、保存先は、組み込む側から受け取る。SDK と Worker の間の取り決めは `protocol.ts` に 1 回だけ書き、Worker を TypeScript にする。

**Tech Stack:** Rust 1.99.1、wasm-bindgen 0.2.129、TypeScript 6、pnpm、@microsoft/api-extractor、Vite（例のビルドだけ）、React（例の中だけ）。

**Spec:** `docs/design/2026-10-24-embedding-api.md`

## Global Constraints

- 公開の入口（`@alto/engine`、`@alto/engine/browser`）に `any` を残さない。
- SDK のソースに、利用者向けの日本語の文言（エラー、警告、意図名）を残さない。Rust が SDK に返す文言も同じ。
- 組み込む側の DOM の要素を、エンジンが置き換えたり動かしたりしない。
- クラッシュ時の安全性（ジャーナル、2 世代のチェックポイント、非常用の書き出し）を弱めない。
- 保存場所の名前：1〜128 文字、`[A-Za-z0-9._/-]` のみ。既定は `default`。
- 表示倍率の上限 `maxPixelRatio` の既定は 2。
- 機能テスト（Metal と SwiftShader）と B1〜B8 で、悪化がないこと。
- 互換は不要（未発表）。`.alto` と復旧記録の形式も変えてよい。変換は書かない。

## Review Focus

- ストロークの途中で Worker が落ちる → `ToolSession.end()` が `WORKER_CRASHED` で拒否され、`doc` はそのまま次の編集を受け付ける。（Task 7 のテスト）
- 大きさ 0 の入れ物（`display: none`）で起動する → 落ちずに 1×1 で起動し、見えた時点で描画が追従する。（Task 7 のテスト）
- 保存場所の名前が空・129 文字・`..` などの不正な文字を含む → `INVALID_ARGUMENT`（`reason: "namespace"`）。（Task 8 のテスト）
- 編集の途中で `dispose()` → 待っている `Promise` は `WORKER_UNAVAILABLE` で拒否され、確定済みの編集は次の起動で復旧できる。（Task 7 のテスト）
- 同じページの 2 つのエンジンの片方が落ちる → もう片方の描画と編集は影響を受けない。（Task 8 のテスト）

---

## 段階 1：約束の範囲を決める

### Task 1: 意図を識別子にし、取り消しの記録を構造にする（Rust）

**Files:**
- Modify: `crates/alto-doc/src/lib.rs`（`Transaction`、`IntentLogEntry`、`Editor::transact`、`log_tx`、取り消し・やり直しの記録）
- Modify: `crates/alto-doc/src/version.rs`（`IntentSummary`、`summarize`、テスト）
- Modify: `crates/alto-engine/src/{lib.rs,edit.rs,versions.rs}`（`transact` の引数、組み込みの意図名、ジャーナルの記録）
- Modify: `crates/alto-wasm/src/lib.rs`（`Request` に `label`）

**Interfaces:**
- Produces:
  - `Transaction { intent: String, #[serde(default, skip_serializing_if = "Option::is_none")] label: Option<String>, … }`
  - `IntentLogEntry { seq, intent, label: Option<String>, #[serde(default)] undone: bool, targets }`
  - `IntentSummary { intent, label: Option<String>, undone: bool, count, targets, target_names }`
  - `Editor::transact(&mut self, intent: Intent, ops, base_revision)`。`pub struct Intent { pub id: String, pub label: Option<String> }` と `impl From<&str> for Intent`。
  - 組み込みの意図の識別子：`image.crop`、`image.rotate`、`image.flip`、`image.resize`、`layer.fill`、`layer.gradient`、`layer.stamp`、`layer.smudge`、`layer.rasterize`、`layer.merge`、`version.restore`（`label` は版の `message`）。

- [ ] 取り消した編集の記録は、`intent` に接頭辞（`"取り消し: "`）を付ける代わりに `undone: true` にする。やり直しは `undone: false` の記録を足す。`version.rs` の `intent_log_summarises_and_follows_undo` を、この形で書き直す（`assert!(e.intent_log()[0].undone)`、`intent == "brush.stroke"`）。
- [ ] テストの意図名と文書名の日本語は、英語の識別子に置き換える。
- [ ] `cargo test --workspace` が通ること。

### Task 2: 警告の種類と、差分の詳細を構造にする（Rust）

**Files:**
- Create: `crates/alto-engine/src/warning.rs`
- Modify: `crates/alto-engine/src/{lib.rs,text.rs,versions.rs}`、`crates/alto-doc/src/version.rs`、`crates/alto-wasm/src/lib.rs`

**Interfaces:**
- Produces:
  - `#[derive(Serialize, Clone, Debug, PartialEq)] pub struct Warning { pub code: &'static str, pub details: serde_json::Map<String, Value> }`
  - 種類：`RECOVERED_OLDER_GENERATION`、`RECOVERED_TRUNCATED`、`RECOVERY_DIVERGED`、`MISSING_TILE {hash}`、`FONT_EMBEDDING_RESTRICTED {family, level: "restricted" | "preview_and_print"}`、`FONT_MISSING {family}`。前の 2 つは Worker が作る。
  - `DiffItem::ContentChanged { id, name, kind, detail: ContentDetail }`。`ContentDetail` は `#[serde(tag = "kind", rename_all = "snake_case")]` の列挙：
    - `Vector { paths: [usize; 2], commands: [usize; 2] }`
    - `Text { text: Option<[String; 2]>, style: bool, writing_mode: Option<WritingMode>, frame: bool, tate_chu_yoko: bool }`
    - `Adjustment`
    - `Unsupported`
  - 削除済みのノード名（今は `"（削除済み）"`）は `names` に入れない。SDK 側では `undefined` になる。
- [ ] Rust が返すエラーの文言（`text.rs` の埋め込みの禁止、`versions.rs` の「前の版から変更がありません」）を英語にし、コードは今のまま（`INVALID_ARGUMENT` など）にする。
- [ ] `cargo test --workspace` と、両方のターゲットでの clippy が通ること。

### Task 3: SDK をモジュールに分け、型を付ける

**Files（`packages/alto-sdk/src/`）:**
- `errors.ts`：`ErrorCode`、`AltoError`（`code`、`details`、`cause`）、`fromWorkerError(message: string): AltoError`（`"CODE: detail"` を読み分ける）
- `events.ts`：`class Emitter<E>`（`on<K extends keyof E>(name: K, fn: (v: E[K]) => void): () => void`）
- `intents.ts`：`BuiltInIntent`（文字列の和）、`type Intent`、`intentRecord(intent: Intent): { intent: string; label?: string }`
- `types/nodes.ts`：`DocNode`、`RasterNode`、`GroupNode`、`TextNode`、`VectorNode`、`AdjustmentNode`、`UnsupportedNode`、`NodeProps`、`BlendMode`、`Effect`（種類ごとの和）、`LayerMask`
- `types/text.ts`、`types/vector.ts`、`types/versions.ts`（`CommitView`、`DiffItem`、`ContentDetail`、`VersionStatus`）、`types/assets.ts`（`ImageAsset`、`FontAsset`：中身の見えない型、`width`、`height`、`info`）
- `snapshot.ts`：`DocumentSnapshot`、`DocumentChange`、`class SnapshotStore { apply(state: StateUpdate): DocumentChange; get(): DocumentSnapshot }`
- `transaction.ts`：`Transaction`（今の `TransactionBuilder` の関数に型を付けたもの）
- `document.ts`：`DocumentHandle`（6.4〜6.6、6.8 の文書側）
- `tools.ts`：`ToolSession`、`BrushOptions`
- `engine.ts`：`createEngine`、`interface Engine`、内部の `EngineCore`
- `internal.ts`：`const internals = new WeakMap<Engine, EngineCore>()`。`unstable.ts` と `testing.ts` だけが使う。
- `unstable.ts`：`unstable(engine)` → `{ settle, readTile, readTileLevel, vectorStats, setGlyphAtlas, onFrame, onDiagnostic, setVisibleTileJobs, enablePrefetch, resetRenderCache, setProfiling, setDecodeWorkers }`
- `testing.ts`：`testing(engine)` → `{ crash, destroyGpu, setWorkerURL, pendingRequests, recoveryStore, corruptJournalTail, corruptCheckpoint, corruptCheckpointCrc, corruptBothCheckpoints, legacyCheckpoint, looseCheckpoint, injectStorageFault, saveBytes, validateFrame, cpuReference, stallDecodeWorkers, view }`
- `browser.ts`：6.8 の補助（Task 9 で中身を作る。ここでは今の `storage.*` を移す）
- `index.ts`：公開するものの再輸出だけ
- `package.json` の `exports`：`"."`、`"./browser"`、`"./unstable"`、`"./testing"`

**Interfaces:** 型と関数の名前は、設計文書の 6.1〜6.10 に書いたとおり。

- [ ] `DocumentHandle.command`、`AltoEngine.request`、`stream`、`mutate`、`apply` を公開しない。今 `command` を通っている操作は、次の名前の付いた関数にする。
  - 選択範囲：`selection.apply`、`selection.clear`、`selection.invert`、`selection.all`
  - マスク：`masks.fromSelection`、`masks.addWhite`、`masks.remove`
  - 効果：`effects.set`
  - 版：`versions.restore`
- [ ] `transact(intent: Intent, build)`。SDK の中の意図名を識別子にする。
  - テキスト：`text.edit`、`text.input`、`text.style`、`text.tate-chu-yoko`
  - ベクター：`vector.edit`、`vector.mask`、`vector.clip`
- [ ] 警告は `AltoWarning[]` で受け取る。Worker の 2 つの日本語の警告を、`RECOVERED_OLDER_GENERATION` と `RECOVERED_TRUNCATED` に置き換える。
- [ ] SDK の日本語のエラー文を、英語の開発者向けの説明にする。`details` を付ける：
  - `CONFLICT` の `reason`：`tool-active`、`folder-changed`、`folder-occupied`、`another-tab`
  - `NOT_FOUND` の `node`
- [ ] Task 3 の時点では、`AltoEngine.create({ canvas })` の起動経路を残してよい（Task 7 で置き換える）。ただし、公開するのは `createEngine` だけにする。
- [ ] `pnpm build` が通ること。

### Task 4: テストと Playground を新しい API に移す

**Files:**
- `tests/e2e/*.js`
- `scripts/verify-browser.mjs`
- `examples/browser-playground/*.js`（`main.js` と `m5-ui.js`〜`m8-ui.js`）

**手順:**
- [ ] `engine.unstable.*` と `engine.request(…)` を、`unstable(engine)` と `testing(engine)` に置き換える。
- [ ] `doc.view.node(id)` を `doc.getSnapshot().nodes.get(id)` に、`doc.view.*` をスナップショットの値に置き換える。
- [ ] `engine.on("frame" | "diagnostic" | …)` を、新しいイベントか `unstable(engine).onFrame` に置き換える。
- [ ] Playground に `messages.ja.ts` 相当の表（`examples/browser-playground/messages.js`）を置く。エラーの `code`、警告の `code`、意図の識別子から、日本語の文言を引く。
- [ ] 使われていない `examples/browser-playground/worker.js` を消す。
- [ ] `node scripts/verify-browser.mjs --functional` が、Metal と SwiftShader で全件成功すること。

### Task 5: 公開 API の一覧を CI で検査する

**Files:**
- `packages/alto-sdk/package.json`（devDependencies に `@microsoft/api-extractor`）
- `packages/alto-sdk/api-extractor.json`
- `packages/alto-sdk/api/alto-engine.api.md`、`packages/alto-sdk/api/alto-engine-browser.api.md`
- `scripts/check.sh`

**手順:**
- [ ] `pnpm --dir packages/alto-sdk api` で一覧を作る。
- [ ] `check.sh` では `api-extractor run`（`--local` なし）を流し、一覧との差があれば失敗させる。
- [ ] 一覧に `any` が現れないことを、`check.sh` で `grep` して確かめる。

## 段階 2：主導権を組み込む側に渡す

### Task 6: 表示位置の API

**Files:** `viewport.ts`、`engine.ts`、`tools.ts`

**Interfaces:**
- Produces（`engine.viewport`）：`get(): Viewport`、`set(next: Partial<Viewport>)`、`zoomAt(clientX, clientY, factor)`、`panBy(dxCss, dyCss)`、`fit(options?: { padding?: number })`（`padding` の既定は 32 CSS ピクセル）、`screenToDocument`、`documentToScreen`。
- `Viewport.zoom` は CSS のピクセル単位。Worker には、デバイスのピクセル単位に直して送る。

**手順:**
- [ ] テスト：
  - `documentToScreen(screenToDocument(p))` が、回転 0.3 と倍率 2.5 で p に戻る（誤差 1e-6）。
  - `zoomAt` の前後で、指定した点の下の文書の位置が変わらない。
  - `fit` の後、文書の四隅が入れ物の内側に入る。
- [ ] 開く・作るときは `fit` する。復旧と Worker の再起動のときは、表示位置を保つ。

### Task 7: 入れ物の要素、状態、再起動をまたぐ操作口

**Files:** `engine.ts`、`document.ts`、`tools.ts`

**Interfaces:**
- Produces：
  - `createEngine(options: EngineOptions): Promise<Engine>`
  - `Engine.element: HTMLCanvasElement`
  - `Engine.status: EngineStatus`
  - `Engine.document: DocumentHandle | null`
  - `Engine.retry(): Promise<void>`
  - `Engine.dispose(): Promise<void>`
  - イベント：`status`、`document`、`viewport`、`saveState`、`recoveryAvailable`、`error`

**手順:**
- [ ] 入れ物の中にキャンバスを作る（`width: 100%; height: 100%; display: block; touch-action: none`）。再起動では、入れ物の中で差し替える。
- [ ] 大きさは `ResizeObserver` で追う。大きさ 0 のときは 1×1 で送る。画面の倍率は `matchMedia("(resolution: …dppx)")` で追う。
- [ ] `assets.baseURL` を受け取り、Worker をそこから読む。ページと違うオリジンなら `INVALID_ARGUMENT`（`reason: "cross-origin-assets"`）。省いたときは `new URL("./worker.js", import.meta.url)`。
- [ ] `window` の `resize` は使わない。`pagehide` では、内部の後始末だけを行う。
- [ ] 再起動の後も、`DocumentHandle` は同じオブジェクトを使う。中のスナップショットを差し替え、購読者に `DocumentChange`（全ノードを `updated` として）を送る。
- [ ] 使用中のツールは、再起動で `WORKER_CRASHED` として終わらせる。
- [ ] `dispose()` は、書き込み待ちのジャーナル（`durable`）を待ってから止める。待つのは最大 2 秒まで。
- [ ] テスト（機能テストに追加）：
  - `testing(engine).crash()` の後、同じ `doc` で編集でき、表示位置が保たれる。
  - ストロークの途中で落としたら、`end()` が `WORKER_CRASHED` で拒否され、その後の編集は成功する。
  - 入れ物の幅を CSS で変えると、`unstable(engine)` のフレームの大きさが追従する。
  - `display: none` の入れ物で起動できる。
  - 編集中の `dispose()` で、待っている `Promise` が `WORKER_UNAVAILABLE` で拒否される。
  - 組み込む側の要素（入れ物）が、置き換えられていない（`===` で比べる）。

### Task 8: 保存場所の名前、記録なしの起動、復旧の API

**Files:**
- `packages/alto-sdk/src/storage-core.js`（段階 3 で TypeScript にする）
- `worker.js`
- `engine.ts`、`document.ts`

**Interfaces:**
- Produces：
  - `storage: { namespace?: string; recovery?: boolean }`
  - `listRecoveryNamespaces(): Promise<{ namespace: string; bytes: number; modified: number }[]>`
  - `deleteRecovery(namespace: string): Promise<void>`
  - `engine.recovery`：`available`、`restore()`、`discard()`、`exportEmergency(): Promise<Blob>`、`recoverAfterStorageFailure(): Promise<RecoveryReport>`

**手順:**
- [ ] OPFS のフォルダを `alto-engine/ns-<名前の UTF-8 を 16 進にしたもの>/` にする。鍵の名前は `alto-engine:<名前>` にする。保存の Worker は、`init` で名前を受け取る。
- [ ] `recovery: false` では、ジャーナルの書き込みと鍵を省く。`durable()` はすぐに解決する。
- [ ] 復旧できる記録が残っているあいだは、`createDocument`、`open`、`openFolder` を `CONFLICT`（`reason: "recovery-pending"`）で拒む。
- [ ] テスト：
  - 名前の違う 2 つのエンジンが同じページで動き、片方を `crash()` しても、もう片方の編集とピクセルが変わらない。
  - 同じ名前の 2 つ目は `CONFLICT`（`namespace-in-use`）で失敗する。
  - 不正な名前（空、129 文字、`a b`、`..`）は `INVALID_ARGUMENT`（`reason: "namespace"`）で失敗する。
  - `discard()` の後に `createDocument` ができる。
  - `deleteRecovery` で、名前が `listRecoveryNamespaces` から消える。
- [ ] 既存の機能テストを、テストの組ごとに別の名前（`test-functional`、`test-m5` など）で動かす。

### Task 9: 保存先、画像の書き出し、対応の判定

**Files:**
- `browser.ts`、`document.ts`、`engine.ts`
- `worker.js`
- `crates/alto-wasm/src/lib.rs`（`exportRgba(scale, x0, y0, x1, y1) -> Vec<u8>`）
- `crates/alto-engine/src/edit.rs`（`export_rgba` を範囲と倍率に対応させる）

**Interfaces:**
- Produces：
  - `ArchiveTarget`、`ArchiveSource`
  - `doc.save(target)`、`engine.open(source)`
  - `doc.exportImage(options?: { type?: "image/png" | "image/webp"; scale?: number; region?: Rect }): Promise<Blob>`
  - `browser.ts` の補助：`fileHandleTarget`、`downloadTarget`、`pickSaveTarget`、`blobTarget`、`fileSource`、`pickFolder`、`opfsFolder`
  - `isSupported(): Promise<{ ok: boolean; reason?: "webgpu" | "offscreen-canvas" | "web-locks" | "opfs" }>`

**手順:**
- [ ] 本体の `save` は、保存先を必ず受け取る。ファイル選択の画面は `pickSaveTarget` だけが出す。
- [ ] `exportImage`：Worker で `exportRgba` を呼び、`OffscreenCanvas` の `convertToBlob` で符号化する。`scale` は 0 より大きく 1 以下で、それ以外は `INVALID_ARGUMENT`。
- [ ] テスト：
  - 4 色の文書を `exportImage()` で書き出して `createImageBitmap` で読み、各色の画素が `readTile` の値と ±1 で一致する。
  - `scale: 0.5` の大きさが半分になる。
  - `blobTarget` で保存して `open` で開き直すと、スナップショットのハッシュが一致する。

## 段階 3：SDK と Worker の取り決めに型を付ける

### Task 10: `protocol.ts` と Worker の TypeScript 化

**Files:**
- Create: `packages/alto-sdk/src/protocol.ts`
- Rename: `worker.js` → `worker.ts`、`storage-core.js` → `storage-core.ts`、`storage-session.js` → `storage-session.ts`、`decode-worker.js` → `decode-worker.ts`、`storage-worker.js` → `storage-worker.ts`
- Modify: `tsconfig.json`（`include` に Worker を足し、`lib` に `WebWorker` を足す）、`build.mjs`（JS のコピーをやめる）、`storage-session.test.js`（`dist` の JS を読む）

**Interfaces:**
- Produces：
  - `interface Requests { [name]: { args: …; result: … } }`。今ある要求をすべて書く。
  - `type Reply = { t: "reply"; id: number; ok: true; value: unknown } | { … ok: false; error: { code: ErrorCode; message: string } }`
  - Worker からの通知の型の和：`patch`、`frame`、`storageFailure`、`gpuLost`、`gpuRestored`、`diagnostic`、`recoveryAvailable`
  - `EngineCore.request<K extends keyof Requests>(t: K, args: Requests[K]["args"], transfer?): Promise<Requests[K]["result"]>`

**手順:**
- [ ] `tsc` が Worker を含めて通ること。
- [ ] `node --test` が通ること。
- [ ] 機能テストが全件成功すること。

## 段階 4：手引き・例・`1.0` の準備

### Task 11: 手引きと例

**Files:**
- `docs/embedding.md`
- `examples/minimal-embed/`（`index.html`、`main.ts`、`package.json`、`vite.config.ts`）
- `examples/react-embed/`（同じ構成）
- `scripts/check-examples.sh`

**手順:**
- [ ] 手引きの中身は、設計文書の 7 章のとおり。
- [ ] `minimal-embed`：素の TypeScript で 150 行以内。新規作成、ブラシ、取り消し、PNG の書き出し、保存と読み込み、復旧の表示を含む。
- [ ] `react-embed`：`useSyncExternalStore` でレイヤーの一覧を表示するだけの小さな例。
- [ ] `check-examples.sh`：両方の例を `vite build` し、`verify-browser.mjs` の新しい組 `embed` で、ビルドした `minimal-embed` を開く。描く、取り消す、書き出す、保存して開き直したらハッシュが一致する、を確かめる。
- [ ] CI の手順に `check-examples.sh` を加える。
- [ ] README の SDK の節を、手引きへの案内にする。

### Task 12: 記録と検証

- [ ] Rust のテスト、両方のターゲットでの clippy、WASM の再ビルド、`pnpm build`、`check.sh`。
- [ ] 機能テスト（Metal、SwiftShader）。
- [ ] B1〜B8 を、変更前と交互に 5 回ずつ計測する。
  - 計測のコードが新しい API に移るので、変更前のビルドでは、移す前の計測のコードを使う。
  - 計測の中身が同じであることは、差分を見て確かめる。
- [ ] 結果を `verification/EMBEDDING-API-RESULTS-2026-10-25.md` に書く。
- [ ] `SHA256SUMS` と `build.json` を更新し、段階ごとにコミットする。
