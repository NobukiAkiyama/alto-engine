# Alto Engine を Web アプリに組み込む

この文書は、Alto Engine の SDK（`@alto/engine`）を、ほかの Web アプリに組み込む人のための手引きです。動く例は次の 2 つです。

- [`examples/minimal-embed`](../examples/minimal-embed)：素の TypeScript で書いた、いちばん小さいエディタ
- [`examples/react-embed`](../examples/react-embed)：React のアプリへの組み込み

公開 API の一覧は、[`packages/alto-sdk/api/alto-engine.api.md`](../packages/alto-sdk/api/alto-engine.api.md) と [`alto-engine-browser.api.md`](../packages/alto-sdk/api/alto-engine-browser.api.md) にあります。この一覧は CI で検査しています。

## 1. 入口

| 入口 | 中身 | 互換の約束 |
|---|---|---|
| `@alto/engine` | `createEngine`、文書、編集、ツール、表示位置、保存、エラー、イベント、型 | あり |
| `@alto/engine/browser` | ファイル選択、ダウンロード、フォルダ選択、OPFS のフォルダ | あり |
| `@alto/engine/unstable` | 毎フレームの統計など、診断用の機能 | なし。予告なく変わる |
| `@alto/engine/testing` | クラッシュや障害を起こす、エンジン自身のテスト用の機能 | なし。製品のコードからは使わない |

## 2. 最小の組み込み

```ts
import { createEngine, isSupported } from "@alto/engine";
import { downloadTarget } from "@alto/engine/browser";

if (!(await isSupported()).ok) throw new Error("WebGPU が使えません");

const engine = await createEngine({
  container: document.querySelector<HTMLElement>("#editor")!,
  storage: { namespace: "drawings/42" },
});
const doc = engine.recovery.available
  ? await engine.recovery.restore()
  : await engine.createDocument({ width: 2048, height: 1536 });

const layer = await doc.transact("layer.add", (tx) => tx.addRaster({ name: "Layer 1" }));
doc.subscribe(() => render(doc.getSnapshot()));

editor.addEventListener("pointerdown", (event) => {
  const stroke = doc.tools.begin("brush", { layer, color: [0, 0, 0, 1] });
  stroke.input(event);
  // pointermove で stroke.input(e)、pointerup で stroke.end()
});

await doc.save(downloadTarget("work.alto"));
await engine.dispose();
```

- **入れ物の要素（`container`）は、組み込む側のもの**です。エンジンはその中に自分のキャンバスを 1 つ置きます（`engine.element`）。組み込む側の要素を置き換えたり動かしたりはしません。
- **大きさは入れ物に追従します。** CSS で入れ物の大きさを決めてください。窓の大きさの変化ではなく、要素の大きさの変化に追従します。非表示（大きさ 0）のまま起動してもかまいません。
- **入力のイベントは、入れ物に付けてください。** Worker を作り直すとき、エンジンはキャンバスを差し替えます。入れ物に付けたイベントは、そのまま使えます。

## 3. バンドラー

SDK は、Worker と WASM を `new Worker(new URL("./worker.js", import.meta.url), { type: "module" })` の形で読み込みます。この形は、Vite と webpack 5 がそのまま理解し、Worker と WASM をまとめて出力します。

**Vite：** Worker が、ほかのモジュールの読み込みと、入れ子の Worker の起動を行うため、Worker の出力形式を ES モジュールにします。

```ts
// vite.config.ts
export default defineConfig({
  worker: { format: "es" },
});
```

**webpack 5：** 追加の設定は要りません。

**バンドラーを使わない場合：** `packages/alto-sdk/dist/` を、そのままページと同じオリジンから配信してください。ファイルを別の場所に置くときは、`createEngine({ assets: { baseURL: "/static/alto/" } })` で場所を教えます。Worker はページと同じオリジンからしか読み込めないため、違うオリジンを渡すと `INVALID_ARGUMENT`（`reason: "cross-origin-assets"`）で失敗します。

## 4. 配信の条件

- **対応ブラウザ：** WebGPU、`OffscreenCanvas`、Web Locks、OPFS を持つブラウザです（Chrome と Edge の現行版）。`isSupported()` は、足りないものを `reason` で返します（`"webgpu"`、`"offscreen-canvas"`、`"web-locks"`、`"opfs"`）。
- **Content-Security-Policy：** 次を許可してください。COOP / COEP（cross-origin isolation）は要りません。
  - `worker-src 'self'`（バンドラーが Worker を `blob:` にする設定なら `blob:` も）
  - `script-src 'self' 'wasm-unsafe-eval'`
- **メモリの目安（4K・20 レイヤーの計測、B6）：**
  - GPU：最大で約 485 MiB。既定の予算は 512 MB で、`gpu.memoryBudgetMB` で変えられます。
  - WASM：エンジン本体と展開用の Worker を合わせて、上限は約 222 MiB。
  - エンジンを複数置くと、そのぶん増えます。

## 5. エンジンの状態

`engine.status` と `status` イベントで、次の状態を知らせます。

| `state` | 意味 | 組み込む側がすること |
|---|---|---|
| `starting` | 起動中 | 待つ |
| `ready` | 動作中 | — |
| `restarting` | Worker の停止から、自動で立て直している | 編集の操作を止めて待つ |
| `storage-failed` | 復旧記録に書けなくなった。読み取りと書き出しはできる | `engine.recovery.exportEmergency()` で今の文書を退避し、`recoverAfterStorageFailure()` で立て直す |
| `failed` | 立て直せなかった | `engine.retry()` を試す |
| `disposed` | 終了した | — |

- **文書の操作口（`DocumentHandle`）は、Worker を作り直しても同じオブジェクトのままです。** 入れ替わるのは、`open`、`createDocument`、`recovery.restore` を呼んだときだけです。そのとき `document` イベントが届きます。
- **表示位置も、作り直しの前後で変わりません。**
- **使用中のストロークは、作り直しで終わります。** `end()` が `WORKER_CRASHED` で失敗します。
- **戻る・進むのキャッシュ（bfcache）：** ページがキャッシュに入ると（`pagehide` の `persisted` が真）、記録を持つエンジンは Worker を止め、保存場所の名前の鍵を手放します。そのあいだ、ほかのタブで同じ文書を開けます。ページが戻ると、鍵を取り直し、記録から立て直します（状態は `restarting` から `ready`）。鍵がほかのタブに取られていれば `failed`（`CONFLICT`、`namespace-in-use`）になり、`engine.retry()` で再試行できます。記録を持たないエンジンは、Worker を止めずにページと一緒に止まります。ページを閉じたとき（`persisted` が偽）は、エンジンを終了します。

## 6. 保存場所の名前（namespace）

- 復旧用の記録（ジャーナル）は、`storage.namespace` ごとに分かれます。名前が違えば、同じサイトで複数のエディタを同時に開けます。
- 同じ名前のエンジンは、ブラウザ全体で同時に 1 つだけです。2 つ目は `CONFLICT`（`reason: "namespace-in-use"`）で失敗します。別のタブで同じ文書を開いた場合も同じです。
- 名前は 1〜128 文字で、使える文字は `A-Z a-z 0-9 . _ - /` です。文書ごとの名前（`"drawings/42"` など）を勧めます。
- `storage: { recovery: false }` にすると、記録を書きません。閲覧専用の画面や、組み込む側が自分で保存を管理する場合に使います。クラッシュすると、保存していない編集は失われます。
- 使われなくなった記録は、`listRecoveryNamespaces()` で一覧にし、`deleteRecovery(name)` で消します。使用中の名前は消せません。

### 復旧

```ts
if (engine.recovery.available) {
  // 利用者に選んでもらう
  const doc = restore ? await engine.recovery.restore() : (await engine.recovery.discard(), await engine.createDocument(meta));
}
```

**記録が残っているあいだは、`createDocument`、`open`、`openFolder` は `CONFLICT`（`reason: "recovery-pending"`）で失敗します。** 新しい文書が、保存していない編集の記録を黙って上書きしないためです。先に `restore` か `discard` を呼んでください。

### 編集の確定と、記録への書き込み

- `transact` などの編集の `Promise` は、**文書に反映された時点**で解決します。
- 記録への書き込みは、それとは別に進みます。書き込みの完了は `await doc.durable()` で待てます。
- `dispose()` は、書き込みを最大 2 秒待ってから終了します。

## 7. 文書を読む

```ts
const snapshot = doc.getSnapshot();      // 凍結済み。変わらなければ同じオブジェクト
const node = snapshot.nodes.get(id);     // node.type: "raster" | "group" | "text" | "vector" | "adjustment" | "unsupported"
doc.subscribe((change) => {
  change.nodes.added; change.nodes.updated; change.nodes.removed; // 変わったノードの ID
});
```

- 変わっていないノードは、前のスナップショットと同じオブジェクトです。React の `memo` などで、変わったノードだけを描き直せます。
- `subscribe` と `getSnapshot` は、React の `useSyncExternalStore` にそのまま渡せます（[`examples/react-embed`](../examples/react-embed)）。

## 8. 編集の意図（intent）

`transact` の第 1 引数は、取り消し履歴と版の記録に残る「何のための編集か」の識別子です。表示する文言ではありません。

```ts
await doc.transact("layer.add", (tx) => tx.addRaster());
await doc.transact({ id: "app:stamp", label: "スタンプ" }, (tx) => /* … */);
```

- 組み込みの識別子は、`BuiltInIntent` 型にあります（`layer.add`、`brush.stroke`、`text.input` など）。
- 組み込む側が独自に作る編集は、`app:` で始まる識別子にします。必要なら `label` に表示用の文字列を残せます。
- 取り消し履歴や版の記録を表示するときは、識別子から自分の言語の文言を引いてください。版の記録（`CommitView.intents`）には、`intent`、`label`、`undone`（取り消した編集かどうか）が入ります。

## 9. エラーと警告

```ts
try {
  await engine.createDocument(meta);
} catch (e) {
  if (e instanceof AltoError && e.code === "CONFLICT" && e.details?.reason === "recovery-pending") { /* … */ }
}
```

- 判断には `code`（`ErrorCode`）と `details` を使ってください。`message` は開発者向けの英語の説明で、利用者に見せることは想定していません。
- 警告（`snapshot.warnings`、フォントの取り込み結果の `warnings`）も、`code` と `details` の組です（`RECOVERED_TRUNCATED`、`FONT_EMBEDDING_RESTRICTED` など）。
- どの呼び出しにも結びつかない失敗（ストロークの入力の途中の失敗など）は、`error` イベントで届きます。
- 日本語の文言の表の例は、[`examples/browser-playground/messages.js`](../examples/browser-playground/messages.js) にあります。

## 10. 保存・読み込み・書き出し

- **`.alto` ファイル：** `doc.save(target)` で保存し、`engine.open(blobOrSource)` で開きます。SDK の本体は、ファイル選択の画面を出しません。`@alto/engine/browser` の次の補助を使えます。
  - `pickSaveTarget()`：保存先を選ぶ画面を出す
  - `fileHandleTarget()`：ファイルハンドルに書く
  - `downloadTarget()`：ダウンロードにする
  - `blobTarget()`：メモリ上に受け取る
- **フォルダ形式（`.altoproj`）：** `doc.saveFolder(provider)` と `engine.openFolder(provider)` で扱います。独自の保存先（サーバーなど）は `ObjectStorageProvider` を実装します。
- **画像：** `doc.exportImage({ type, scale, region })` で、画面に表示されるのと同じ画素を PNG か WebP で書き出します。サムネイルには、`scale` を 1 より小さくします。`scale` が 1/2 以下のときは、縮小済みの表示タイル（ピラミッドの段）から読むので、読み戻しが段ごとに 4 分の 1 になります。

## 11. 表示位置

倍率（`zoom`）は CSS のピクセル単位です。1 は、文書の 1 ピクセルが CSS の 1 ピクセルに対応することを表します。

```ts
engine.viewport.zoomAt(e.clientX, e.clientY, 1.1); // その点を中心に拡大
engine.viewport.panBy(dx, dy);                     // ドラッグで移動
engine.viewport.fit();                             // 全体を表示
const [x, y] = engine.viewport.screenToDocument(e.clientX, e.clientY);
```

文書を開いたとき・作ったときは、全体が見えるように合わせます。

## 12. 版の約束

- 版番号は semver に従います。`1.0.0` までは、互換を壊す変更が `0.x` の版で入ることがあります。
- `1.0.0` の後は、何かをなくす前に、少なくとも 1 つの版で `@deprecated` にします。
- `.alto`、フォルダ形式、復旧記録には、それぞれ形式の番号があります。新しい SDK は、古い形式のものも開けます。
