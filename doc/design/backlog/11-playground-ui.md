# Playground と UI

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**状態：除外（2026-11-04、利用者の決定）。** 本書の項目は実装しない。後で範囲に戻すときの記録として残す。

**Goal:** SDK にある機能を Playground から操作できるようにし、UI 部品と包みの範囲を決める。

**Architecture:** SDK は画面を持たない。Playground は作り方の例（`examples/browser-playground`）。ビルドのないプレーンな JS で、`index.html` と `*-ui.js` に分かれている。UI の作業の前に ui-design スキルを読む。

## 現状

- `index.html` の合成モードの選択肢は Normal、Multiply、Screen（393〜395行）。
- `image-versions-ui.js`：スタンプと指先は座標の数値入力のみ。
- レベル補正、トーンカーブ、色相・彩度の UI がない。

## UI-1 スタンプと指先のドラッグ

**設計。** `data-tool` に `stamp`、`smudge` を追加する。

- スタンプ：Alt+クリックで取得元を置き、ドラッグでストロークの点列を集める。離したときに `doc.image.cloneStamp` を1回呼ぶ。
- 指先：同じくドラッグの点列を集めて `doc.image.smudge` を呼ぶ。
- 点列は `MAX_STROKE_POINTS`（4096）で打ち切らず、超えたら点を間引く。確定は CPU の処理なので、ストローク中の表示は線のオーバーレイだけとする。座標は `viewport.screenToDocument` からレイヤーのノード座標へ変換する。

## UI-2 効果の操作画面

**設計。** 「ぼかし・効果設定」に効果の種類の選択を加える。

- レベル補正：5つの数値とヒストグラムの表示（`doc.image.histogram`）。
- トーンカーブ：SVG の曲線エディタ。2〜16 点、ドラッグ、チャネル選択。
- 色相・彩度、反転：スライダーとチェック。
- 既存の一覧（順序変更、削除）へ統合。02-effects の新しい効果も同じ形で足す。

## UI-3 合成モードの選択肢

`BlendMode` の17種を `index.html` の select へ追加する。設定欄の値は既に文字列で渡している。README の説明（31行目、217行目）と MILESTONES の注記を同じ PR で直す。

## UI-4 エディタ UI 部品

**設計。** 埋め込みの設計では対象外。作るなら別パッケージ `@alto/ui` で、レイヤーパネル、ツールバー、カラーピッカーを Web Components として提供する。`doc.subscribe`、`getSnapshot` の上に作る。D1 の判断待ち。製品を作る側の要件が決まるまでは着手しない。

## UI-5 React、Vue の包み

**設計。** `examples/react-embed` が `useSyncExternalStore` に載ることを示している。`@alto/react` は `useAltoEngine`、`useDocument`、`useSnapshot(selector)` の薄い層で、100 行程度に収まる。Vue も同じ形。公開するとメンテナンスの対象が増えるので、D1 の判断待ち。

## 実装タスク

### Task UI-3: 合成モードの選択肢

**Files:** Modify `examples/browser-playground/index.html`、`README.md`、`doc/MILESTONES.md`。

- [ ] 17種を追加する。
- [ ] ブラウザ試験 `every blend mode in the playground select is a valid mode`。
- [ ] `sh scripts/run.sh` で起動し、Overlay で見た目が変わることを画面で確認。
- [ ] PR。

### Task UI-1: スタンプと指先のドラッグ

**Files:** Modify `examples/browser-playground/index.html`、`main.js`、`image-versions-ui.js`。

- [ ] ブラウザ試験 `dragging the stamp tool calls cloneStamp once with the dragged points`。
- [ ] ブラウザ試験 `stamp without a source explains what to do`。
- [ ] 試験 `a long drag is thinned below the point limit`。
- [ ] 実装し、回転とズームをかけた表示で位置が合うことを画面で確認。PR。

### Task UI-2: 効果の操作画面

**Files:** Modify `examples/browser-playground/index.html`、`m5-ui.js`。

- [ ] ブラウザ試験 `curves editor sends strictly increasing points`、`levels fields refuse in_black above in_white`。
- [ ] 実装し、画面で確認。PR。

## Review Focus

- ドラッグの途中でレイヤーの選択が変わる場合。
- 非表示のレイヤーやロック中のレイヤーでのエラーの表示。
- 長い処理中の二重クリック。
- 回転した表示とレイヤー変形の組み合わせの座標変換。
- 入力欄に不正な値が残ったときに、SDK のエラーが UI で読めること。
