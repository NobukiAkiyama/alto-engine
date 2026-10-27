# 組み込み用 API の設計（2026-10-24）

## 1. 目的

Alto Engine を、ほかの Web アプリに部品として組み込めるようにする。今の SDK（`@alto/engine`）は、同梱の Playground を動かすために育ってきたもので、組み込む側から見ると次の点で扱いにくい。

- どこまでが約束（公開 API）で、どこからが内部なのかが分からない。
- エンジンが、ページの DOM、窓の大きさ、終了の時機、保存場所、表示する文言を自分で決めてしまう。

この作業では、SDK を「組み込む側が頼れる約束」に作り直す。エンジンの描画・文書・保存の中身は変えない。

**前提：** SDK は未発表で、使っているアプリや人はいない（2026-10-24 時点）。そのため互換を気にせず、API の形を作り直してよい。`.alto` ファイルと復旧記録の形式も、まだ外に出ていないので変えてよい。

## 2. 目標・目標外・失敗とみなすこと

### 目標

- **組み込みが短く書ける。** 起動、文書を開く、描く、保存、終了までが 30 行程度で書けること（5 章の例）。Worker、WASM、OPFS、Web Locks を知らなくても組み込めること。
- **公開するものはすべて型が付いている。** 公開 API に `any` がない。内部の要求名（`"transact"` など）を組み込む側が書く場面がない。
- **主導権は組み込む側にある。** DOM の配置、大きさ、終了の時機、保存場所の名前、表示する文言、入力の振り分けは、組み込む側が決める。
- **約束が守られていることを機械で確かめる。** 公開 API の一覧を自動で生成してリポジトリに置き、意図しない変更を CI で止める。
- **手引きと最小の例がある。** 一般的なバンドラー（Vite、webpack）で組み込めることを CI で確かめる。

### 目標外（今回はやらない）

- **画面の部品（レイヤーパネル、ツールバー、色の選択など）。** SDK は画面を持たない。部品は組み込む側が作る。Playground は、その作り方の例として残す。
- **React や Vue 向けの包み。** 7 章の例で、`subscribe` と `getSnapshot` だけで React の `useSyncExternalStore` に載ることを示すにとどめる。
- **ブラウザ以外への組み込み。** Rust のクレートを、ネイティブのアプリへ直接組み込む使い方は対象外。
- **1 つのエンジンで複数の文書を同時に開くこと。** エンジン 1 つにつき文書は 1 つ。複数の文書は、エンジンを複数作って扱う（6.3）。
- **描画・文書・保存の性能の改善。** 測定（B1〜B8）で悪化がないことだけを確かめる。

### 失敗とみなすこと（動いていても）

- 使いやすさのために、クラッシュ時の安全性（ジャーナル、2 世代のチェックポイント、障害時の非常用の書き出し）を弱めること。
- 組み込む側が持っている DOM の要素を、エンジンが置き換えたり動かしたりすること。
- 特定のバンドラーやフレームワークがないと組み込めないこと。
- 公開 API から内部の要求に直接届く抜け道を残すこと（`request`、`stream`、`command`）。

## 3. 制約

- 対象のブラウザは、WebGPU と `OffscreenCanvas` を持つもの（Chrome / Edge の現行版）。今と同じ。
- Worker はページと同じオリジンから読み込む必要がある（ブラウザの制約）。CDN から直接 Worker を起動することはできない。
- `SharedArrayBuffer` は使わない。組み込む側に cross-origin isolation（COOP / COEP）の設定を求めない。
- キャンバスの制御を Worker に渡す操作（`transferControlToOffscreen`）は、1 つのキャンバスにつき 1 回しかできない。Worker を作り直すときは、キャンバスも作り直す必要がある。
- SDK と WASM は一緒に配布する。両者の間の取り決め（要求と返信の形）は内部のものとし、公開 API の約束には含めない。

## 4. 今の SDK の問題（コードで確認したもの）

| 問題 | 場所 | 組み込む側で困ること |
|---|---|---|
| 内部への入口が公開されている | `AltoEngine.request(t, payload: any)`、`stream`、`mutate`、`DocumentHandle.command(type, payload: any)`、`apply` | どこまで頼ってよいか分からない。内部を変えると組み込み側が壊れる |
| 型が `any` | `index.ts` 全体で 36 か所。イベントの中身、`FontAsset.record`、`AssetRef.record`、`setAdjustment`、`effects.set`、`openDocument(source: … \| any)`、`save(target: … \| any)` | 型の検査が効かない |
| イベント名が文字列 | `on(name: string, fn: (v: any) => void)`。名前は 14 種類（`frame`、`gpuLost`、`canvasReplaced` など） | 綴りの誤りに気づけない。何が届くか分からない |
| キャンバスの要素を置き換える | Worker の再起動で、`canvas.cloneNode` と `replaceWith` を使う（`performRestart`） | React などが持っている要素が勝手に入れ替わる |
| 再起動のたびに文書の操作口が作り直される | `performRestart` が `new DocumentHandle` を作る | 組み込む側が持っている `doc` が古いまま残る |
| 窓のイベントを自分で拾う | `window` の `resize` と `pagehide` | 窓ではなく、要素の大きさが変わったときに追従できない |
| 復旧の保存場所と同時実行の鍵が 1 つに固定 | OPFS の `alto-engine` フォルダ、鍵 `alto-engine-working-journal` | 同じサイトで、2 つ目のエディタや別の文書を同時に開けない |
| 文言が日本語で固定 | エラー（`"レイヤーが見つかりません"` など）、警告（`"最後の不完全な記録を除いて復旧しました"`）、取り消し履歴の名前（`"テキスト編集"`、`"縦中横"`） | 組み込む側の言語や文言で出せない。履歴の名前は文書に保存されてしまう |
| ファイル選択の画面を SDK が出す | `save()` に保存先を渡さないと `showSaveFilePicker` を開く。`storage.pickFolder` | 組み込む側の画面の流れに合わない |
| 開くと表示位置が初期化される | `openDocument`、`recover`、再起動の後に `viewport.set({ zoom: 1, … })` | 再起動のたびに、利用者が見ていた位置が失われる |
| 表示倍率が 2 種類ある | `viewport.zoom` はデバイスのピクセル単位、`getCssZoom` は CSS のピクセル単位 | どちらを使えばよいか分からない |
| 変更の通知が粗い | `change` イベントは Worker の返信をそのまま渡す。`node()` と `nodes` は、呼ぶたびに複製して凍結する | 何が変わったかが分からず、画面を全部描き直すことになる |
| 画像として書き出せない | エンジンには `export_rgba` があるが、SDK から呼べない | サムネイルや PNG の書き出しが作れない |
| 診断用と試験用の機能が混ざっている | `unstable` に、GPU の破棄、障害の注入、チェックポイントの破壊が並ぶ | 製品のコードから誤って呼べる |
| 1 つのファイルにすべてある | `index.ts` 1519 行 | 読みにくく、公開範囲の管理もしにくい |

## 5. 組み込みの全体像

組み込む側のコードは、次のようになる。

```ts
import { createEngine, AltoError } from "@alto/engine";
import { downloadTarget } from "@alto/engine/browser";

const engine = await createEngine({
  container: document.querySelector<HTMLElement>("#editor")!,
  storage: { namespace: "drawings/42" },
});
engine.on("status", ({ state, error }) => showStatus(state, error));

const doc = engine.recovery.available
  ? await engine.recovery.restore()
  : await engine.createDocument({ width: 2048, height: 1536 });

doc.subscribe((change) => renderLayerPanel(doc.getSnapshot(), change));

const layer = await doc.transact("layer.add", (tx) => tx.addRaster({ name: "Layer 1" }));

engine.element.addEventListener("pointerdown", (event) => {
  const stroke = doc.tools.begin("brush", { layer, color: [0, 0, 0, 1], size: 24 });
  stroke.input(event);
  const move = (e: PointerEvent) => stroke.input(e);
  addEventListener("pointermove", move);
  addEventListener("pointerup", () => {
    removeEventListener("pointermove", move);
    void stroke.end();
  }, { once: true });
});

await doc.save(downloadTarget("work.alto"));
await engine.dispose();
```

## 6. 設計

### 6.1 入口の分け方とファイルの構成

パッケージの入口を 4 つに分ける（`package.json` の `exports`）。

| 入口 | 中身 | 約束 |
|---|---|---|
| `@alto/engine` | `createEngine`、文書、編集、ツール、表示位置、保存、エラー、イベント、型 | 版の約束の対象（6.12） |
| `@alto/engine/browser` | ブラウザの画面を使う補助：ファイル選択、ダウンロード、フォルダ選択、OPFS のフォルダ | 版の約束の対象 |
| `@alto/engine/unstable` | 診断：毎フレームの統計、描画の段階の切り替え、タイルの読み戻し、ベクターの統計 | 予告なく変わる |
| `@alto/engine/testing` | 試験用：Worker のクラッシュ、GPU の破棄、障害の注入、チェックポイントの破壊、旧形式の書き出し | 予告なく変わる。製品のコードからは使わない |

- `unstable` と `testing` は、エンジンの内部に届く関数を受け取る形にする（例：`import { testing } from "@alto/engine/testing"; testing(engine).crash()`）。エンジンの公開型には現れない。
- `index.ts` を分ける：`engine.ts`、`document.ts`、`transaction.ts`、`tools.ts`、`viewport.ts`、`storage.ts`、`errors.ts`、`events.ts`、`types/`（文書のノード、テキスト、ベクター、版管理）、`protocol.ts`（6.11、内部）。
- 今ある `AltoEngine` クラスは公開しない。公開するのは `createEngine` と、型としての `Engine`。組み込む側が `new` したり、継承したりする場面をなくす。

### 6.2 起動と終了

```ts
interface EngineOptions {
  container: HTMLElement;
  storage?: { namespace?: string; recovery?: boolean };
  gpu?: { powerPreference?: "low-power" | "high-performance"; memoryBudgetMB?: number };
  assets?: { baseURL?: string | URL };
  maxPixelRatio?: number;
}
function createEngine(options: EngineOptions): Promise<Engine>;
```

- **キャンバスはエンジンが持つ。** 組み込む側は入れ物の要素（`container`）を渡す。エンジンはその中に自分のキャンバスを 1 つ置き、`engine.element` として見せる。Worker を作り直すときは、入れ物の中で自分のキャンバスを差し替える。組み込む側の要素には触らない。`canvasReplaced` イベントはなくす（入力のイベントは `engine.element` ではなく入れ物に付けるよう、手引きで勧める）。
- **大きさは `ResizeObserver` で追う。** 入れ物の大きさが変わったら追従する。窓の `resize` は使わない。画面の倍率（`devicePixelRatio`）の変化は `matchMedia` で追う。上限は `maxPixelRatio`（既定 2。今と同じ）。
- **`pagehide` は残す。** ページを離れるときに、ジャーナルを閉じて鍵を手放すため。これは安全のために必要で、組み込む側に任せない。ただし `dispose` は呼ばず、内部の後始末だけを行う。
- **`dispose()` は `Promise` を返す。** 書き込み待ちのジャーナルを書き終えてから Worker を止め、鍵を手放し、キャンバスを入れ物から外す。
- **状態は 1 つの値で表す。** `engine.status` と `status` イベント：

```ts
type EngineState = "starting" | "ready" | "restarting" | "storage-failed" | "failed" | "disposed";
interface EngineStatus { state: EngineState; error: AltoError | null }
```

  - `restarting`：Worker の停止や GPU の喪失から、自動で立て直している途中。編集は受け付けない。
  - `storage-failed`：ジャーナルに書けなくなった。読み取りと非常用の書き出し（6.8）はできる。編集は受け付けない。
  - `failed`：立て直せなかった。`engine.retry()` でやり直せる。
  - 今ある `recovering`、`recovered`、`engineFailure`、`storageFailure`、`gpuLost`、`gpuRestored`、`canvasReplaced` の各イベントは、`status` 1 つにまとめる。
- **文書の操作口は、再起動をまたいで同じものを使う。** Worker を作り直しても、`engine.document` は同じオブジェクトのままで、中身だけを立て直す。操作口が入れ替わるのは、組み込む側が開く・作る・復旧するを呼んだときだけ。そのときは `document` イベントを出す。

### 6.3 保存場所の名前と、同時に開ける数

- **復旧用の記録は、名前（`storage.namespace`）ごとに分ける。** OPFS の `alto-engine/<名前>/` に置き、同時実行の鍵も `alto-engine:<名前>` にする。名前を省いたときは `default`。
  - 名前は 1〜128 文字。英数字と `. _ - /` だけを使える。フォルダ名には、`/` などをそのまま使えないので符号化して使う（符号化の方法は実装する人に任せる）。
- **同じ名前のエンジンは、同時に 1 つだけ。** 別のタブや同じページですでに使われていれば、`createEngine` は `CONFLICT` で失敗する。詳細に `{ reason: "namespace-in-use", namespace }` を付ける。
- **名前が違えば、同じページに複数のエンジンを置ける。** エンジンごとに Worker と GPU のデバイスを持つので、メモリを多く使う。手引きに、1 つあたりの目安（B6 の値）を書く。
- **復旧の記録を持たない起動もできる。** `storage: { recovery: false }`。ジャーナルを書かず、鍵も取らない。閲覧専用の画面や、組み込む側が自分で保存を管理する場合のため。クラッシュすると、保存していない編集は失われる。このことを型の説明と手引きに書く。
- **名前ごとの後始末。** 組み込む側が多くの文書を扱う場合、使われなくなった記録が OPFS に残る。そのため次の関数を用意する。
  - `listRecoveryNamespaces(): Promise<{ namespace: string; bytes: number; modified: number }[]>`
  - `deleteRecovery(namespace): Promise<void>`（使用中なら `CONFLICT`）

### 6.4 文書を読む

```ts
interface DocumentHandle {
  getSnapshot(): DocumentSnapshot;
  subscribe(listener: (change: DocumentChange) => void): () => void;
  // …編集の関数（6.5）
}
interface DocumentSnapshot {
  readonly revision: number;
  readonly meta: DocumentMeta;
  readonly root: NodeId;
  readonly nodes: ReadonlyMap<NodeId, DocNode>;
  readonly selection: SelectionInfo | null;
  readonly history: { undo: number; redo: number };
  readonly versions: VersionStatus;
  readonly dirty: boolean;
  readonly warnings: readonly AltoWarning[];
}
interface DocumentChange {
  readonly revision: number;
  readonly nodes: { added: NodeId[]; updated: NodeId[]; removed: NodeId[] };
  readonly meta: boolean;
  readonly selection: boolean;
  readonly history: boolean;
  readonly versions: boolean;
  readonly dirty: boolean;
}
```

- **読み取りは同期で、凍結済みのスナップショットを返す。** 変わっていないノードは、前のスナップショットと同じオブジェクトを使い回す。React の `memo` などで、変わったノードだけを描き直せる。呼ぶたびに複製する今の `node()` はなくす。
- **ノードの型は種類で分ける。** `DocNode = RasterNode | GroupNode | TextNode | VectorNode | AdjustmentNode`。`kind` の値で見分け、共通の性質（名前、表示、ロック、不透明度、合成方法、変形、マスク、効果）は共通の型にまとめる。ノードの種類は、エンジンの文書モデル（`alto-doc`）にあるものに合わせる。
- **`subscribe` と `getSnapshot` だけで、React の `useSyncExternalStore` に載る形にする。** そのため、`getSnapshot` は変更がなければ同じオブジェクトを返す。
- **時間のかかる問い合わせは非同期のまま。** テキストの当たり判定、カーソル位置、選択範囲の四角形、版の一覧と差分、選択範囲の輪郭、スナップショットのハッシュ。いずれも返り値に型を付ける。
- **取り消し履歴の名前を読めるようにする（できれば）。** 「取り消す：レイヤー追加」のような表示のため、`history` に次に取り消す・やり直す編集の意図（6.5）を入れる。エンジンの履歴は意図を持っているので取り出せるはずだが、手間が大きければ後回しにしてよい。

### 6.5 編集する

```ts
type Intent = BuiltInIntent | `app:${string}` | { id: BuiltInIntent | `app:${string}`; label?: string };
transact<T>(intent: Intent, build: (tx: Transaction) => T): Promise<T>;
```

- **編集の意図は、表示用の文言ではなく識別子にする。** 取り消し履歴と版の記録には、識別子（`layer.add`、`text.edit`、`text.tate-chu-yoko` など）と、組み込む側が付けた任意の `label` を保存する。表示するときは、組み込む側が識別子から自分の言語の文言を引く。`label` は、組み込む側が独自に作る編集（`app:` で始まる識別子）で使う。
  - SDK に組み込みの識別子の一覧（`BuiltInIntent`）を型として置く。今の SDK と Playground が使っている日本語の意図名を、すべて識別子に置き換える。
  - 版の記録（`CommitView.intents`）も、識別子と `label` を持つ形にする。`.alto` と復旧記録の形式が変わるが、未発表なので変換は用意しない。
- **`Transaction` の関数にはすべて型を付ける。**
  - `setProps(id, patch: Partial<NodeProps>)`
  - 調整は種類ごとの型の和にする（例：`{ kind: "levels"; … }`）。
  - 効果も種類ごとの型の和にする（例：`{ kind: "gaussian_blur"; radius: number }`）。
  - マスクの値の型を決める。
  - 種類はエンジンの定義に合わせる。
- **今 `command(type, payload)` を通っている操作には、それぞれ名前の付いた関数を用意する。** 選択範囲、マスク、効果、版の復元が該当する。`command` は公開しない。
- **編集は今と同じく、1 つずつ順番に行う。** 返り値の `Promise` は、編集が文書に反映された時点で解決する。ジャーナルへの書き込みを待つには `doc.durable()` を使う。この 2 つの違いを、型の説明に書く。
- **組み込む側が受け取る資産は、中身の見えない型にする。** 画像やフォントの取り込み結果（今は `record: any` を持つ）は、`ImageAsset`、`FontAsset` という中身の見えない型にする。組み込む側が見られるのは、大きさ、フォント名など、表示に必要な情報だけにする。

### 6.6 ツールと入力

- **入力の振り分けは組み込む側が行う。** エンジンはポインターのイベントを自分で拾わない。今と同じく、組み込む側がツールを始めて、イベントを渡す。
- **ツールの種類と設定には型を付ける。**

```ts
doc.tools.begin("brush" | "eraser", options: BrushOptions): ToolSession;
interface ToolSession {
  input(event: PointerEvent | InputSample): void;
  end(): Promise<void>;
  cancel(): Promise<void>;
}
```

  - `BrushOptions` は今の形をもとにして、型の説明を付ける。設定の項目は、大きさ、硬さ、間隔、不透明度、流量、筆圧の効き方、描く先（レイヤーかマスクか）。
- **ツールの使用中に別の編集を呼んだときの扱いは、今と同じ。** 先にツールを終えてから編集する。この振る舞いを型の説明に書く。
- `PointerEvent` を渡したときは、座標の変換、合体したイベント（coalesced events）の展開、マウスの筆圧の扱いを、今と同じく SDK が行う。

### 6.7 表示位置

```ts
interface Viewport { center: [number, number]; zoom: number; rotation: number }
engine.viewport.get(): Viewport;
engine.viewport.set(next: Partial<Viewport>): void;
engine.viewport.zoomAt(clientX: number, clientY: number, factor: number): void;
engine.viewport.panBy(dxCss: number, dyCss: number): void;
engine.viewport.fit(options?: { padding?: number }): void;
engine.viewport.screenToDocument(clientX: number, clientY: number): [number, number];
engine.viewport.documentToScreen(x: number, y: number): [number, number];
```

- **公開する倍率は CSS のピクセル単位だけにする。** 1 は、文書の 1 ピクセルが CSS の 1 ピクセルに対応することを表す。デバイスのピクセル単位は内部だけで使う。`getCssZoom`、`setCssZoom`、`effectiveDpr` はなくす。
- **`zoomAt`、`panBy`、`fit` を用意する。** ホイールでのズームやドラッグでの移動は、どの組み込み先でも同じ計算になるため。ジェスチャーそのもの（ホイールやドラッグのイベントの処理）は、組み込む側が書く。
- **表示位置は再起動をまたいで保つ。** 文書を開いたとき・作ったときは `fit` する。復旧したときと Worker の再起動の後は、それまでの表示位置を保つ。
- `viewport` イベントで、表示位置の変化を知らせる。ブラシのカーソルなど、組み込む側が重ねて描くもののため。

### 6.8 保存・読み込み・復旧・書き出し

- **保存先は、組み込む側が渡す。** SDK の本体はファイル選択の画面を出さない。

```ts
interface ArchiveTarget { write(stream: ReadableStream<Uint8Array>): Promise<void> }
interface ArchiveSource { read(): Promise<Blob> }
doc.save(target: ArchiveTarget): Promise<void>;
engine.open(source: Blob | ArchiveSource): Promise<DocumentHandle>;
doc.saveFolder(provider: ObjectStorageProvider, options?): Promise<FolderSaveReport>;
engine.openFolder(provider: ObjectStorageProvider): Promise<DocumentHandle>;
```

  - `@alto/engine/browser` に補助を置く。
    - 保存先：`fileHandleTarget(handle)`、`downloadTarget(name)`、`pickSaveTarget(options)`（ファイル選択を出す）、`blobTarget()`（書き終えた後に `.blob` を読める）。
    - 読み込み元：`fileSource(file)`。
    - フォルダ：`pickFolder()`、`opfsFolder(name)`。
  - 保存が成功した時点で、文書の「変更あり」の印が消える。これは今の振る舞いと同じ。
- **復旧は `engine.recovery` にまとめる。**
  - `available: boolean`（読み取り専用）
  - `restore(): Promise<DocumentHandle>`
  - `discard(): Promise<void>`：記録を捨てて新しく始める。今は、この手段がない。
  - **復旧できる記録が残っているあいだは、新規作成と読み込みを拒む。** `createDocument`、`open`、`openFolder` は `CONFLICT`（`reason: "recovery-pending"`）で失敗する。組み込む側は、先に `restore` か `discard` を呼ぶ。今は、新規作成や読み込みが最初のチェックポイントで記録を黙って上書きするので、保存していない編集を失いうる。
  - `restore` は、記録がなければ `NOT_FOUND` で失敗する。
- **保存の障害からの回復も `engine.recovery` に置く。**
  - `exportEmergency(): Promise<Blob>`：`storage-failed` の状態で、今の文書を書き出す。
  - `recoverAfterStorageFailure(): Promise<RecoveryReport>`
- **画像として書き出す（新しい機能）。**
  - `doc.exportImage(options?: { type?: "image/png" | "image/webp"; scale?: number; region?: Rect }): Promise<Blob>`
  - エンジンの `export_rgba` を Worker から呼べるようにし、画像の符号化は `OffscreenCanvas.convertToBlob` で行う。サムネイルにも使う。GPU で合成するか CPU で合成するかは、実装する人に任せる。ただし、表示と同じ結果になること（既存の CPU の参照との比較で確かめる）。

### 6.9 エラーと警告

```ts
type ErrorCode =
  | "INVALID_ARGUMENT" | "CONFLICT" | "NOT_FOUND" | "CORRUPT_DATA"
  | "GPU_LOST" | "GPU_UNSUPPORTED" | "STORAGE" | "INTERNAL"
  | "WORKER_CRASHED" | "WORKER_UNAVAILABLE" | "WORKER_TIMEOUT";
class AltoError extends Error {
  readonly code: ErrorCode;
  readonly details: ErrorDetails | undefined;
}
interface AltoWarning { code: WarningCode; details?: Record<string, string | number> }
type WarningCode = "RECOVERED_OLDER_GENERATION" | "RECOVERED_TRUNCATED" | "MISSING_TILE";
```

- **組み込む側は `code` と `details` で判断する。** `message` は開発者向けの英語の説明で、利用者に見せることは想定しない。利用者向けの文言は、組み込む側が `code` から作る。
- **SDK が出す日本語の文言は、すべて英語の開発者向けの説明に置き換える。** 警告も文言ではなく `AltoWarning` にする。
- **`details` は、扱いを分けるのに必要なものだけにする。** 例：
  - `CONFLICT` の `reason`（`namespace-in-use`、`tool-active`、`folder-changed`、`revision`）
  - `NOT_FOUND` の `node`
  - `STORAGE` の `operation`
- **Rust 側の型付きのエラー（`RenderError`、`TileError`）は、今と同じく `CODE: detail` の形で SDK に届く。** SDK はこれを読み分けて `AltoError` を作る。この読み分けは `protocol.ts` の 1 か所にまとめる。
- **Playground は、`code` から日本語の文言を引く表を自分で持つ。** 組み込む側の手本にする。

### 6.10 イベント

```ts
interface EngineEvents {
  status: EngineStatus;
  document: DocumentHandle | null;
  viewport: Viewport;
  saveState: "saving" | "saved" | "error";
  recoveryAvailable: { records: number };
  error: AltoError;
}
engine.on<K extends keyof EngineEvents>(name: K, listener: (value: EngineEvents[K]) => void): () => void;
```

- **`error` は、どの呼び出しにも結びつかない失敗だけに使う。** たとえば、ツールの入力を送る途中の失敗。呼び出しに結びつく失敗は、その呼び出しの `Promise` が拒否される。
- **毎フレームの統計（`frame`）と診断（`diagnostic`、`workerDiagnostic`）は、`@alto/engine/unstable` に移す。** 公開の約束にしない。
- 文書の変更は、`doc.subscribe`（6.4）で知らせる。エンジンの `change` と `dirty` のイベントはなくす。

### 6.11 SDK と Worker の間の取り決め（内部）

- **要求と返信の形を `protocol.ts` に 1 回だけ書く。** 要求の名前から、引数と返り値の型が決まるようにする。

```ts
interface Requests {
  transact: { args: { intent: IntentRecord; ops: Op[]; baseRevision: number }; result: StateUpdate };
  // …今ある 62 種類の要求すべて
}
```

  SDK の内部の `request` は、`request<K extends keyof Requests>(t: K, args: Requests[K]["args"]): Promise<Requests[K]["result"]>` になる。
- **Worker（`worker.js`）と保存の中核（`storage-core.js`）を TypeScript にする。** 両側が同じ `protocol.ts` で検査されるようにする。評価で最も危ないと指摘した箇所（Worker の中の並行処理の約束）を、型で表す第一歩にもなる。並行処理の作りそのものを直すのは、この作業の範囲外とする。
- 試験用と診断用の要求も `protocol.ts` に書く。ただし公開の入口からは呼べない（6.1）。
- `protocol` の版の番号（今は 4）の照合は、今と同じく残す。

### 6.12 版の約束

- **版番号は semver に従う。** 公開の入口が固まるまでは `0.x` のままにし、そのあいだは互換を壊す変更を `0.x` の次の版として出してよい。手引きと 6.1 の入口がそろい、Playground が公開の入口だけで動いた時点で `1.0.0` にする。
- **公開 API の一覧を自動で作り、リポジトリに置く。** `@microsoft/api-extractor` で、公開の 2 つの入口から一覧（`packages/alto-sdk/api/alto-engine.api.md` など）を作る。CI で作り直して差があれば失敗させる。公開 API を変えるときは、一覧の変更を同じコミットに含める。
- **`1.0` の後に何かをなくすときは、先に非推奨にする。** `@deprecated` を付けた版を少なくとも 1 つ出してから、次のメジャー版でなくす。
- **ファイルの形式の約束は、API とは別に持つ。** `.alto`、フォルダ形式、復旧記録の形式には、それぞれ形式の番号がある。新しい SDK は古い形式を開ける、古い SDK は新しい形式を `CONFLICT` で拒む、という約束を手引きに書く。

### 6.13 配布と組み込みの条件

- **Worker と WASM の場所は、`new URL("./worker.js", import.meta.url)` の形で書く。** Vite と webpack 5 は、この形をそのまま処理できる。入れ子の Worker（展開用、保存用）も同じ形にする（今もそうなっている）。
- **ファイルを別の場所に置く組み込み先のために、`assets.baseURL` を用意する。** Worker と WASM をそこから読む。Worker の制約で、ページと同じオリジンでなければならない。違うオリジンを渡されたら、起動の時点で `INVALID_ARGUMENT` にする。
- **手引きに、必要な Content-Security-Policy を書く。** `worker-src 'self'`（または `blob:` を使わないことの明記）と、`script-src 'wasm-unsafe-eval'`。COOP / COEP は要らない。
- **対応するブラウザの判定を、起動の前に呼べるようにする。** `isSupported(): Promise<{ ok: boolean; reason?: "webgpu" | "offscreen-canvas" | "web-locks" | "opfs" }>`。今は `create` が失敗して初めて分かる。

## 7. 手引きと例

- **`doc/embedding.md`（手引き）。** 次のことを書く。
  - 導入
  - バンドラーの設定（Vite、webpack、バンドラーなし）
  - CSP
  - 対応ブラウザ
  - 状態の遷移（6.2）
  - 保存場所の名前の付け方（6.3）
  - 編集の意図の付け方（6.5）
  - エラーの扱い方（6.9）
  - 保存と復旧の流れ
  - メモリの目安
- **`examples/minimal-embed`（最小の例）。** 素の TypeScript で 150 行以内。新規作成、ブラシ、取り消し、PNG の書き出し、`.alto` の保存と読み込み、復旧の表示を含む。Vite でビルドする。
- **`examples/react-embed`（React の例）。** `useSyncExternalStore` で、レイヤーの一覧を表示するだけの小さな例。React は例の中だけで使い、SDK の依存には入れない。
- **Playground を公開の入口だけで動くように直す。** 診断の画面だけは `unstable` を使ってよい。テストの実行は `testing` を使う。`examples/browser-playground/worker.js` は、古い API を呼んでいて使われていないので消す。
- **公開するすべての型と関数に、TSDoc の説明を書く。** 型の説明から、参照用の文書を作る（TypeDoc）。

## 8. 完成の条件

- 公開の 2 つの入口に `any` がない（api-extractor の一覧と `tsc` で確かめる）。
- 公開 API の一覧がリポジトリにあり、CI がその差を検査している。
- `examples/minimal-embed` と `examples/react-embed` が、CI で Vite でビルドでき、ブラウザで次のことに成功する。
  - 描く、取り消す、PNG を書き出す、保存して開き直したらピクセルが一致する
  - Worker を落としても同じ `doc` で編集を続けられ、表示位置が保たれる
- 同じページで、名前の違う 2 つのエンジンが同時に動き、それぞれ復旧できる。同じ名前の 2 つ目は `CONFLICT`（`namespace-in-use`）で失敗する。
- 入れ物の要素の大きさを変えると、窓の大きさが同じでも描画が追従する。
- SDK のソースに、利用者向けの日本語の文言が残っていない（エラー、警告、意図名）。
- 既存の機能テスト（Metal と SwiftShader）が、`testing` の入口を使う形で全件成功する。B1〜B8 に悪化がない。
- 既存の機能テストを、テストごとに別の保存場所の名前で動かす。前のテストの状態に頼る作りを減らすため（評価で指摘した弱点）。

## 9. 進め方

段階ごとに別のプルリクエストにする。各段階で、機能テストと B1〜B8 を確かめる。

1. **約束の範囲を決める。**
   - 入口を分ける（6.1）。`index.ts` を分割する。
   - 型を付ける（6.4〜6.5、6.9〜6.10）。日本語の文言と意図名を識別子と英語に置き換える。
   - api-extractor を入れる。
   - ここで公開 API の形がほぼ決まる。
2. **主導権を組み込む側に渡す。**
   - 入れ物の要素と `ResizeObserver` を使う。操作口を再起動の前後で同じものにする（6.2）。
   - 保存場所の名前と鍵を分ける（6.3）。
   - 保存先と復旧の API を作る（6.8）。
   - 表示位置の API を作る（6.7）。
   - 画像の書き出しを作る（6.8）。
3. **SDK と Worker の取り決めに型を付ける。**
   - `protocol.ts` を作る。
   - `worker.js` と `storage-core.js` を TypeScript にする（6.11）。
4. **手引き・例・`1.0` の準備（7 章）。**
   - 手引きと例を作り、CI でバンドラーの検査をする。
   - Playground を移す。

## 10. 私が決めたこと（確認してほしい点）

- **画面の部品と React 向けの包みは作らない。** SDK は画面を持たず、例で使い方を示すにとどめる。
- **利用者向けの文言は、組み込む側が作る。** SDK は英語の開発者向けの説明と、エラーの種類（`code`）だけを出す。Playground は日本語の表を自分で持つ。
- **1 つのエンジンで開ける文書は 1 つ。** 複数の文書は、名前の違うエンジンを複数作って扱う。
- **画像の書き出し（PNG / WebP）を新しく加える。** 組み込む先のほとんどが、サムネイルか書き出しを必要とすると考えたため。
- **公開 API の一覧の検査には api-extractor を使う。** 開発用の依存が 1 つ増える。
- **公開する表示倍率は、CSS のピクセル単位だけにする。**
- **文書を開いたとき・作ったときは、表示位置を `fit` に合わせる。** 今の振る舞い（倍率 1 で中央）から変える。
- **復旧できる記録があるあいだは、新規作成と読み込みを拒む（6.8）。** 組み込む側に 1 手間増えるが、保存していない編集を黙って失わないことを優先した。

## 11. 実装する人に任せること

- 保存場所の名前を、OPFS のフォルダ名に符号化する方法。今の `alto-engine/` の直下にある記録（名前のない旧い置き場所）は読まない。消すかどうかも任せる（未発表なので移行は要らない）。
- `unstable` と `testing` の入口が、エンジンの内部に届く方法（モジュールの中だけで見える `WeakMap` など）。公開の型に現れなければよい。
- スナップショットで、変わっていないノードを使い回す仕組みの作り方。Worker の差分（`patch`）から作れるはず。
- 調整、効果、マスクの型の細かい形。エンジンの定義に合わせる。
- 組み込みの意図の識別子の一覧と名前の付け方。`名詞.動詞` の形を勧める。
- 画像の書き出しを、GPU で合成するか CPU で合成するか（6.8 の条件を満たすこと）。
- 例のアプリの見た目。
