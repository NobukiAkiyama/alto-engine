# 最終検証結果


生の検証JSONはこのディレクトリに置かない。新しい実行は CI の artifact `alto-browser-reports`（GPU lifecycle の手動実行は `gpu-lifecycle-<OS>`）、ローカルは gitignore した `verification/out/`。以下のかつてのファイル名は、この変更より前の git 履歴にある。

2026-10-18、同梱ビルドをこのMac上で実行しました。

## 機能

- **Rustテスト18件成功**。原子的Transaction、可逆Op、未知ノードの保持、タイル内容ハッシュ、パッケージ破損検出、保存/ジャーナルの再現、GPU資源管理、WGSL検証を含みます。
- **Chrome 154 / 内蔵Chromium 155：各11件成功**。空文書のGPU validation error scope、合成モード、グループ、変形、GPUブラシとCPU参照の画素比較、タイル境界、筆圧、Undo/Redo、消しゴム、`.alto`の画素・スナップショット一致、保存中断・書き込み途中のWriter Worker強制終了、無効Transaction、GPU喪失、Worker再起動、ジャーナル末尾破損、PNG/JPEG、二重起動拒否を確認。
- native / wasm32のClippyを警告なしで通過。Rust fmt、TypeScriptビルドも成功。
- Chromeで最終版の通常再読み込みと復旧案内を確認。OPFS同期ハンドルは各書き込み後に閉じ、pagehideでWorkerと作業領域のロックを解放します。
- `sample.alto` は別のネイティブEngineで保存→読み込みし、スナップショット一致とZIP CRCを確認。ブラウザでも読み込みと表示を確認。

生データ： Chrome機能（`chrome-functional.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）、Chromium機能（`browser-functional.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）、Chrome性能（`chrome-benchmark.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）、Chromium性能（`browser-benchmark.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）、[Rust/SDKログ](rust-and-sdk.log)。

## B1〜B7

| 指標 | Chrome 154 | Chromium 155 | 判定 |
|---|---:|---:|---|
| B1 パン p95 (ms) | 7.00 | 6.20 | PASS |
| B2 ズーム完了 (ms) | 107.10 | 63.40 | PASS |
| B3 連続描画 p95 (ms) | 7.50 | 5.40 | PASS |
| B4 全面ストロークUndo (ms) | 10.90 | 11.20 | PASS |
| B5 保存 / 読込 (ms) | 278.80 / 983.50 | 287.70 / 1026.80 | PASS |
| B6 GPU / WASM (MiB) | 483.75 / 240.19 | 483.75 / 240.50 | PASS |
| B7 復旧と初回表示 (ms) | 1269.30 | 1410.30 | PASS |

### 測定条件と範囲

- 3840×2160、互いに異なる不透明なラスター20レイヤー。背景色とタイルごとの模様がある文書をテスト側で作成します。Normalで完全に隠れた下位レイヤーを省略する最適化が有効です。
- Chrome：1200×678 CSS px、DPR 2。内蔵Chromium：1093×827 CSS px、DPR 2。SharedArrayBufferなし、`crossOriginIsolated=false`。
- B1：120フレームのパン。Mainのperformance.nowで要求からWorker応答・GPU queue完了まで測ったp95。rAF待機時間は集計外です。
- B2：倍率1→0.5、新レベルの全可視タイルの合成・GPU完了を待ちます。
- B3：直径100pxのブラシを30回連続入力し、各入力から表示のGPU処理完了までのp95。2フレームを33.4msとして判定します。
- B4：直径2048pxのブラシで4K全面を通るストロークを確定してからUndo応答まで。画素の再現性は機能テストでも確認します。
- B5：パッケージのメモリへの符号化 / Blobからの読込とOPFSチェックポイント書き込みまで。ネイティブの保存ダイアログや利用者の外部ディスクへの書込時間は含みません。
- B6：初期表示・連続描画・全面ストロークのプレビュー中と確定後・再読込後をサンプルした最大値。GPUはエンジンが管理するテクスチャ・バッファのサイズ推計、WASMはlinear memoryの確保量です。GPUドライバーの内部確保やブラウザ/JS全体のプロセスメモリは含みません。
- B7：WASMがブラウザキャッシュにある状態でWorkerを再起動し、4K文書の復旧、保存時のハッシュ一致、初回GPU描画完了まで。単なる空文書の起動より多くの処理を含みます。

測定は付属のシナリオでの一回の実行結果です。異なるGPU、半透明20レイヤー、複雑なグループ、長時間運用での同じ性能を保証しません。GPU timestamp-queryによる個別カーネルの時間は測っていません。

## ビルド

WASMは1,907,808 bytes、gzipでは437,465 bytes（wasm-opt未適用）。Mainと2つのWorkerのコード、宣言ファイル、WASMを同梱しています。

SHA-256: `8f242eb571bb0ad23941354d24ef457fd751d542155a67993ae89de89942f40b`

## サンプル

[`sample.alto`](sample.alto) は768×512、背景・色のかたち・ペイントの3レイヤーです。Playgroundの「開く」で読み込み、レイヤーの表示や不透明度、位置を変えて試せます。生成元は `crates/alto-engine/examples/create-sample.rs` です。

設計との差分と後続フェーズは [MILESTONES.md](../MILESTONES.md) を参照してください。
