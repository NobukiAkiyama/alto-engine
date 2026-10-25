# Alto Engine v0.4 — M7・M8

Rust / WASM / WebGPUの画像編集エンジン、TypeScript SDK、編集用Playgroundです。**M7：テキスト（横組み・日本語縦組み・フォントフォールバック・OpenType機能・テキスト選択）**と、**M8：版管理（コミット・意味的差分・フォルダ形式・Git Adapter）**を追加しました。M6までのベクター・選択範囲・ラスター編集・調整・非破壊ぼかしも使えます。文書・Undo/Redo・`.alto`保存・OPFSジャーナル復旧につながっています。

[実装状況](MILESTONES.md) / [最終検証結果](verification/RESULTS.md) / [M7/M8サンプル文書](verification/sample-m78.alto) / [M6サンプル文書](verification/sample-m6.alto) / [lyon・vello比較](verification/VECTOR-COMPARISON.md) / [alto-gitの実行記録](verification/m8-git-transcript.txt)

[コールド表示の実装・測定結果](verification/COLD-RENDERING-RESULTS-2026-10-24.md) / [設計と後続段階](COLD-RENDERING-DESIGN.md)。通常ラスタの可視描画を中断可能にしました。

## まず起動する

ビルド済みWASMとSDKを同梱しています。Python 3があれば、RustやNode.jsを導入せずに試せます。

```sh
cd alto-engine
sh scripts/run.sh
```

ChromeまたはEdgeのWebGPU対応環境で **http://localhost:8765/examples/browser-playground/** を開きます。サーバーはプロジェクトのルートから起動してください。`file://` では動作しません。別ポートは `ALTO_PORT=8766 sh scripts/run.sh` で指定できます。

### 操作

- 「新規」で文書を作成。「画像を追加」でPNG/JPEGをレイヤーとして配置。
- 「ブラシ」「消しゴム」で描画。筆圧は対応するペン入力から取得します。
- 「移動」でドラッグしてパン。ホイールでズーム。右側の「ビュー回転」で表示を回転。
- レイヤーを選択し、表示・ロック・不透明度・Normal/Multiply/Screen・位置・倍率・回転を編集。
- 「＋グループ」で分離グループを作成し、選択したグループに「＋レイヤー」で子を追加。SDKでは既存ノードのグループ間移動もできます。
- 「元に戻す」「やり直す」、またはCmd/Ctrl+Z、Shift+Cmd/Ctrl+Z。
- 「保存 .alto」で保存。「開く」で復元。File System Access API対応環境ではファイルへ書き込み、それ以外ではダウンロードします。
- 異常終了後、同じオリジンで開き直すと「編集を復旧」が表示されます。OPFSの記録はブラウザのサイトデータに保存されます。

「検証と計測」から機能テスト23件、M5テスト8件、M6テスト10件、M7テスト9件、M8テスト6件、4K・半透明20レイヤーのB1〜B7計測、GPUデバイス喪失テストを実行できます。機能テストと計測は専用のテスト文書に切り替わり、現在の作業用ジャーナルも置き換えます。編集中の文書は先に保存してください。「結果を保存 JSON」で測定条件と結果を書き出せます。

## M7の使い方（テキスト・縦組み）

「検証と計測」の「M7/M8サンプルを開く」で、縦組みの本文・横組みの見出し・縦中横の注記と3つの版を含む文書を開けます。上部の「テキスト設定」で設定欄へ移動します。

### フォント

文字の表示には文書に埋め込むフォントが必要です。「フォントを読み込む」でOTF/TTF/TTCを追加するか、「テスト用フォント」で同梱のNoto Sans JP / Noto Sansのサブセット（SIL OFL、収録字は約500）を登録します。ブラウザやOSのフォントは使いません。読み込み時にライセンス（OS/2 fsType）を確認し、埋め込み禁止などの場合は警告します。

「フォント」で選んだ書体を先頭に、登録順に**フォールバック**します。先頭の書体に無い文字（例：欧文用フォントに無い仮名）は次の書体で表示し、どの書体にも無い文字は仮の枠で示します。

### テキストを追加・編集する

1. 本文、組み方向（縦組み/横組み）、サイズ、行送り、字送り、色、揃え、枠（エリア＝折り返し、ポイント＝改行のみ）を設定し、「テキストを追加」。
2. テキストレイヤーを選ぶと設定欄に読み込みます。変更後「テキストを更新」。本文の書き換えは、範囲ごとの書式を保ったまま反映します。
3. 「自動縦中横（桁）」は縦組みで横に並べる半角数字の桁数（0〜4、既定2）。3桁以上の数字や欧文は90°回転します。
4. 「OpenType機能」に `palt`（プロポーショナル詰め）、`liga=0`（合字なし）、`fwid`（全角幅）などをカンマ区切りで指定します。

### キャンバス上で選択・入力する

右の「テキスト」ツールを選び、テキストレイヤー上をドラッグすると文字を選択できます（青い範囲、黄色のキャレット）。そのまま入力・日本語入力の確定・Backspace/Delete・Enter（改行）で編集し、矢印キーで移動します（縦組みでは↑↓が文字送り、←→が行移動）。Shift+矢印で選択を広げ、Cmd/Ctrl+Aで全選択します。

選択中に「選択部分に色・サイズ・機能を適用」で、その範囲だけ書式を変えられます。「選択部分を縦中横」は、任意の文字列（最大8文字、1emに収まらない場合は圧縮）を縦中横にします。文字の編集は1操作ずつUndoできます。

句読点・括弧・長音などの約物は、フォントの縦組み字形（vert/vrt2）で列の右上などへ配置します。縦組み字形を持たないフォントでも、句読点の移動・括弧の回転で代替します。行頭に句読点・閉じ括弧・小書き仮名・長音が来ないよう、行末へ追い出して折り返します（禁則）。

### M7 SDK例

```ts
const jp = await engine.assets.importFont(await (await fetch('NotoSansJP.otf')).blob());
console.log(jp.info.family, jp.warnings); // fsTypeの警告
let id;
await doc.transact('縦組み', tx => {
  tx.addFont(jp);
  id = tx.addText({
    text: '令和8年10月19日、第12回「縦組み」。',
    style: { fonts: [jp.record.id], size: 32, line_height: 1.75, features: [{ tag: 'palt', value: 1 }] },
    writing_mode: 'vertical_rl',
    frame: { kind: 'area', width: 400, height: 600 },
    auto_tcy: 2,
  });
});
await doc.text.replace(id, 0, 2, '平成');          // UTF-16範囲の置換（1回のUndo）
await doc.text.style(id, 0, 2, { color: [0.7, 0.1, 0.1, 1] });
await doc.text.setTateChuYoko(id, 3, 5);         // 指定範囲の縦中横
const offset = await doc.text.hitTest(id, x, y); // 文書座標→文字位置
const rects = await doc.text.selectionRects(id, 0, 5); // 選択の四角形（文書座標）
const next = await doc.text.navigate(id, offset, 'next_line');
const info = await doc.text.info(id); // 行、未表示文字、使用フォント、範囲
```

オフセットはJavaScriptの文字列と同じUTF-16単位です。`replaceText` / `applyTextStyle` はSDKから単独でも使えます。

## M8の使い方（版管理・フォルダ形式・Git）

### コミットと差分

上部の「版管理」で設定欄へ移動します。メッセージを入力して「コミット」すると、現在の文書を1つの版として記録します。前回のコミット以降の編集は「ブラシ ×3（背景）」のように対象レイヤーごとにまとめて残ります。

- 版の一覧で版を選び「選択した版と比較」で、その版の変更をレイヤー単位（追加・削除・移動・属性・画素範囲・テキスト内容）で表示します。「作業中の変更」は最新の版との差分です。画素の変化はタイルのハッシュだけで判定します。
- 「この版に戻す」は選んだ版の内容を現在の文書へ適用します。1回の「元に戻す」で取り消せ、履歴は消えません。
- コミットは `.alto` に保存され、開き直しても履歴と古い版を使えます。

### フォルダ形式（`.altoproj`）

「フォルダに保存」で保存先フォルダを選ぶと、`manifest.json`・`document.json`・`snapshots/`・`commits/`・`objects/` を書きます（Chrome/EdgeのFile System Access APIが必要）。2回目以降は新しいオブジェクトだけを書き、最後に `manifest.json` を置き換えます。別の場所でフォルダが変更されていれば、上書きせずにエラーにします。「フォルダを開く」で読み込みます。

### Gitで管理する（alto-git）

フォルダ形式はそのままgitに入れられます。Altoのコミットをgitのコミットに写すには、同梱のCLIを使います。ビルドした `target/release/alto-git` をPATHへ置くか、パスを指定して実行します（`git` コマンドが必要です）。

```sh
cargo build --release -p alto-git
cd 作品のリポジトリ
alto-git sync art.altoproj          # 未反映のAltoコミットを1つずつgitコミットへ
alto-git log art.altoproj           # 版ごとの意図とレイヤー単位の変更
alto-git diff HEAD~1:art.altoproj HEAD:art.altoproj
alto-git install .                  # git diff で document.json をレイヤー単位で表示
git diff HEAD~1 -- art.altoproj/document.json
alto-git export work.alto art.altoproj   # 単一ファイル → フォルダ
alto-git pack art.altoproj work.alto     # フォルダ → 単一ファイル
```

gitのコミットはその版の時点のフォルダで、メッセージにIntentの要約と変更レイヤーの一覧、`Alto-Commit:` トレーラを含みます。`sync` は何度実行しても同じ版を二重に写しません。[実行例](verification/m8-git-transcript.txt)

### M8 SDK例

```ts
const v1 = await doc.versions.commit('下地', { author: 'mimi' });
// …編集…
const v2 = await doc.versions.commit('線画を描く');
const log = await doc.versions.log();               // 新しい順
const { items, names } = await doc.versions.diff(v1.hash, v2.hash);
await doc.versions.restore(v1.hash);                 // Undo可能
const folder = await engine.storage.pickFolder();    // または opfsFolder('名前') / new MemoryObjectProvider()
await doc.saveFolder(folder);
const reopened = await engine.openFolder(folder);
```

独自の保存先は `ObjectStorageProvider`（`getObject` / `putObjects` / `list` / `readRef` / `writeRef`）を実装して使えます。SDK・Worker・WASMは**v0.4 / プロトコル4**で照合します。

## M6の使い方

「検証と計測」の「M6サンプルを開く」で、曲線・塗り・線、Even-oddの穴、ベクターマスク、グループのクリッピングパスを組み合わせた文書を開けます。上部の「ベクター設定」から各操作へ移動できます。

### パスを作成・編集する

1. 「長方形を追加」「楕円を追加」「曲線を追加」でベクターレイヤーを作成します。「新規ベクター」は入力中のSVGパスデータを使います。
2. レイヤーと「パス」を選び、塗り・線の色、線幅、線端、結合、パスの不透明度、Non-zero / Even-oddを設定して「パスを更新」。SVGのM/L/H/V/Q/T/C/S/A/Z（相対指定も可）を入力できます。ファイル全体のSVG読み込みではなく、`d` 属性のパスデータが対象です。
3. 「パスを追加」「パスを削除」で一つのレイヤー内のパスを増減できます。
4. 「パスの点を編集」で白い端点・黄色い制御点を表示し、ドラッグして編集します。確定した変更は一回のUndoで戻せます。SVGパスデータ欄で座標を直接編集する方法も使えます。
5. 「表示倍率 %」またはホイールで拡大縮小。レイヤーの位置・倍率・回転、グループ、不透明度、合成モード、ラスターのクリッピング、M5のマスク・ぼかしも適用できます。

パスはf64の編集データとして保存します。曲線をメッシュに変換し、4x MSAAのR8カバレッジから描画します。表示レベルごとにメッシュと表示タイルを再生成するため、拡大時も元のパスから描き直します。端点・制御点のドラッグ中は点を動かし、離すと曲線と文書へ確定します。

### ベクターマスク・クリッピングパス

- 入力したパスを「パスをマスクに設定」で、選択レイヤー・グループ・調整レイヤーのマスクにできます。ぼかし幅は0〜64文書px。「ベクターマスクを有効」で切り替え、「ベクターマスクを外す」で解除します。
- 「クリッピングパスを設定」はぼかしのないパス境界です。有効切替・解除ができ、ベクターマスクや既存のR8マスクと重ねると交差領域だけが表示されます。
- 「マスクのパスを読み込む」「クリップのパスを読み込む」で既存の形を入力欄に戻し、編集後に再設定できます。既存の独立変換は再設定時も維持します。
- 両方とも選択ノードの座標で保持し、親グループの移動・拡大縮小・回転にも追従します。SDKではマスク/クリップ固有のAffine2も設定できます。元のラスター・パスは変えません。

### M6 SDK例

```ts
const data = await engine.vectors.parseSvg('M80 240 Q240 0 400 240 L400 400 H80 Z');
let vector;
await doc.transact('ベクター追加', tx => {
  vector = tx.addVector([{
    data, fill: [0.3, 0.5, 1, 1], fill_rule: 'non_zero',
    stroke: { color: [1, 1, 1, 1], width: 6, cap: 'round', join: 'round' },
  }], { name: '曲線' });
});
const mask = await engine.vectors.parseSvg('M0 0 H300 V512 H0 Z');
await doc.vectors.setMask(vector, { path: mask, feather: 8 });
await doc.vectors.setClipPath(vector, { path: mask });
await doc.vectors.setMask(vector, null); // ベクターマスクを解除
await doc.undo();
```

`tx.setVector` / `setVectorMask` / `setClipPath` は同一Transactionにまとめられます。`engine.vectors.toSvg(data)` でパスデータへ戻せます。

### lyon / velloの比較

[比較レポート](verification/VECTOR-COMPARISON.md)に実測と判断を記録しています。比較ページは `http://localhost:8765/examples/vector-comparison/`。別WASMを同梱し、velloを本体へ含めずに再実行できます。比較用ソースを再ビルドする場合は `sh scripts/build-vector-comparison.sh` を使用します。

## M5の使い方

### 選択範囲

画面上部の「長方形選択」「楕円選択」「投げ縄選択」を選び、キャンバスをドラッグします。「操作」で置き換え・追加・削除・共通部分を指定し、ぼかし幅0〜64pxで境界を柔らかくできます。白黒の点線が現在の境界です。

「全選択」「選択反転」「選択解除」は独立したボタンです。Cmd/Ctrl+Aで全選択、Cmd/Ctrl+Dで解除できます。「選択なし」では全域を描けます。「空の選択範囲」では描けないので、再び描くには解除してください。ブラシ・消しゴム・マスク用ブラシは選択範囲に制限されます。

### マスクとクリッピング

上部の「マスク・クリップ設定」で設定欄へ移動できます。

- レイヤーまたはグループを選び、「選択をマスク化」または「白マスクを追加」。マスクを外すと元画像が戻ります。
- 「マスクを編集」を有効にし、黒で隠す・白で表示を選んでブラシで塗ります。マスク編集中の消しゴムは黒で隠します。元のRGBA画素は変えません。
- 「マスクを有効にする」で表示効果を切り替えます。「レイヤーと連動」を外すと、その位置を保って独立します。再連動しても位置は保ちます。
- 「下のレイヤーにクリップ」は、同じグループの直前の通常レイヤーを基底とするクリッピングチェーンです。基底の半透明度を保ちます。基底がないクリップは表示しません。

マスク・選択範囲はR8タイル、マスク変換はf64で保持します。マスクにもUndo/Redoが使えます。

### ぼかしと色調整

「ぼかし・効果設定」で半径0〜64のGaussianぼかしを追加します。半径は**文書座標のpx（カーネルの打ち切り半径）**です。標準偏差は半径/3。レイヤー・グループに適用でき、一覧で半径の変更・順序変更・削除ができます。選択範囲がある状態で追加すると、その領域をエフェクト用のR8マスクとして取り込みます。その後の選択変更で適用範囲は変わりません。

「色調整設定」から、明るさ・コントラスト・彩度の調整レイヤーを追加できます。同じグループ内の自分より下の内容だけが入力です。調整レイヤーを選んで数値を変え、「選択中の調整を更新」で反映します。不透明度・Normal/Multiply/Screen・マスクも使用できます。

ぼかしは必要な周辺画素を含むROIを用意し、GPUの縦横2パスで計算します。元のタイルは変更せず、エフェクトの設定のみを保存します。

「M5サンプルを開く」で、これらを組み合わせた編集可能なサンプルを開けます。テストとサンプルの読み込みは現在の作業用文書・ジャーナルを切り替えるので、作業中の文書は先に保存してください。

## 構成

| パス | 役割 |
|---|---|
| `crates/alto-core` | UUID、f64幾何、BLAKE3、タイル座標 |
| `crates/alto-raster` | 256pxタイル、LZ4内容ストア、CPUブラシ参照実装 |
| `crates/alto-vector` | f64パス、kurbo、lyon、スタイル、メッシュ準備trait |
| `crates/alto-text` | フォント、rustybuzzシェーピング、横組み/縦組みの組版、グリフ→パス、選択 |
| `crates/alto-doc` | Scene Graph、原子的Transaction、可逆Op、Undo/Redo、スナップショット、コミット・意味的差分 |
| `crates/alto-store` | 内容アドレス付きオブジェクト、`.alto` ZIPとフォルダ形式、破損検出 |
| `crates/alto-gpu` | wgpu、GPU合成・ブラシ、表示タイルキャッシュ、復旧 |
| `crates/alto-engine` | 文書・履歴・タイル・アセット・保存の統合 |
| `crates/alto-wasm` | Engine Worker内のWASMランタイム |
| `crates/alto-git` | Git Adapter（エンジン外のCLI）：Altoコミット→gitコミット、レイヤー単位の差分 |
| `packages/alto-sdk` | TypeScript SDK v0.4、Engine Worker、OPFS Storage Worker、フォルダ保存先 |
| `examples/browser-playground` | 編集UIとビルド済みWASM |
| `tests/e2e` | 実ブラウザで実行する画素検証・性能測定 |
| `tests/fonts` | テスト・デモ用フォント（Noto Sans JP / Noto Sans のサブセット、SIL OFL 1.1） |

Mainは入力と読み取り専用ミラー、Engine Workerは文書・GPU、Storage Workerは同期アクセスハンドルによるOPFS記録を担当します。SharedArrayBuffer、COOP/COEPは不要です。画像や編集内容を外部サーバーへ送信する処理はありません。

同梱の `verification/sample.alto` を「開く」から読み込むと、3レイヤーのサンプルを試せます。

## SDKの最小例

```ts
import { AltoEngine } from './packages/alto-sdk/dist/index.js';

const engine = await AltoEngine.create({ canvas });
const doc = await engine.createDocument({ width: 1920, height: 1080 });
let layer;
await doc.transact('レイヤー追加', tx => {
  layer = tx.addRaster({ name: 'ペイント' });
});
const stroke = doc.tools.begin('brush', {
  layer,
  tip: { size: 40, hardness: 0.8 },
  color: [0.2, 0.4, 0.8, 1],
});
stroke.input({ x: 100, y: 100, pressure: 1 });
stroke.input({ x: 300, y: 200, pressure: 0.5 });
await stroke.end(); // GPU読み戻し・Transaction・ジャーナルflush
await doc.undo();
await doc.redo();
await doc.save(); // ブラウザの保存先選択またはダウンロード
```

Transactionコールバックは同期で記述し、更新完了はPromiseで待ちます。`doc.view.node(id)` は凍結した読み取り専用の値です。独自保存先は `ArchiveStorageProvider` の `readArchive` / `writeArchive` を実装できます。型定義は `packages/alto-sdk/dist/index.d.ts` を参照してください。`unstable` APIは検証用です。

`viewport.zoom` は物理ピクセル/文書ピクセルです。画面上の倍率を指定する場合は `engine.viewport.setCssZoom(1)`、読み取る場合は `engine.viewport.getCssZoom()` を使います。実効DPRは `engine.effectiveDpr`（最大2）で確認できます。Workerが停止した場合、`engine.lifecycleState` と `engine.engineFailure` を確認し、`engine.retryWorker()` で確定済みの作業記録から復旧できます。`recovering` 中は編集を停止し、`recovered` の通知後に `engine.document` を取得し直してください。保存障害は従来どおり別の `storageFailure` で通知されます。

## M5 SDK例

```ts
await doc.selection.apply({
  kind: 'ellipse', x: 100, y: 100, width: 300, height: 200, feather: 8,
});
await doc.effects.set(layer, [{ type: 'gaussian_blur', radius: 12 }]);
await doc.selection.clear(); // エフェクトが取り込んだ範囲は維持
await doc.masks.addWhite(layer);
const maskStroke = doc.tools.begin('brush', {
  layer, target: 'mask', color: [0, 0, 0, 1], tip: { size: 64 },
});
maskStroke.input({ x: 240, y: 180, pressure: 1 });
await maskStroke.end();
await doc.transact('クリッピング', tx => tx.setProps(layer, { clip_to_below: true }));
```

`doc.masks.fromSelection(layer)` / `remove(layer)`、`tx.addAdjustment(...)` / `setAdjustment(...)`、`mask_enabled` / `mask_linked`、`doc.selection.outline()` も利用できます。

## ソースから再ビルド

Rust **1.99.1**（`rust-toolchain.toml`）、`wasm32-unknown-unknown`、wasm-bindgen-cli **0.2.129**（`crates/alto-wasm/Cargo.toml` の `wasm-bindgen` と同じピン）、Node.js、pnpmが必要です。依存バージョンはCargo.lockとpnpm-lock.yamlで固定しています。`sh scripts/check.sh` はリリース wasm を組み直し、`examples/browser-playground/pkg` と `packages/alto-sdk/dist/pkg` が一致しなければ失敗します。

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked
sh scripts/build.sh
sh scripts/check.sh
```

`ALTO_TARGET_DIR=/任意のビルド先 sh scripts/build.sh` でRustの成果物を外に置けます。ブラウザ検証は画面から実行でき、`ALTO_BROWSER_REPORT_DIR=/任意の出力先 node scripts/verify-browser.mjs --functional` でChrome・ローカルサーバーの起動からM4〜M8の機能検証まで自動実行できます。`--functional` を外すとB1〜B7の性能測定も実行します。CIは機能検証を必須とし、性能測定はハードウェア依存のため別に扱います。ネイティブRustテストだけではWebGPU・OPFSの実動作を検証できません。

DPRの性能比較は同じCSS表示サイズで、`ALTO_BROWSER_DPR=1` と `=2` を指定し、`ALTO_BROWSER_VIEW_MODE=css` で同じ文書範囲を描画します。例: `ALTO_BROWSER_DPR=2 ALTO_BROWSER_WIDTH=1093 ALTO_BROWSER_HEIGHT=827 ALTO_BROWSER_VIEW_MODE=css ALTO_BROWSER_PROFILE=1 ALTO_BROWSER_REPORT_DIR=/tmp/alto-dpr2 node scripts/verify-browser.mjs --benchmark-only`。従来の同じ物理ズームでの比較には `ALTO_BROWSER_VIEW_MODE=physical` を使います。結果JSONにはキャンバスのCSS/物理寸法とB2時点の可視文書範囲を記録します。

## 現在の範囲

sRGB / RGBA8、R8マスク、ベクターパスの塗り/線、ベクターマスク、クリッピングパス、丸ブラシ・消しゴム、明るさ・コントラスト・彩度、Gaussianぼかし、横組み・縦組みのテキスト（埋め込みフォント）、一本の履歴の版管理とフォルダ形式・gitへの写像が対象です。ルビ・圏点、グリフアトラス、版の分岐・マージ、ブラウザ内のgit操作などの継続課題、各機能の上限は [MILESTONES.md](MILESTONES.md) に記載しています。画像編集ソフトの操作をこの正本の上へ足す設計は [IMAGE-EDITING.md](IMAGE-EDITING.md) です。
