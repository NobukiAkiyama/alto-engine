# 中断可能なコールド可視描画の実装と検証


生の検証JSONはこのディレクトリに置かない。新しい実行は CI の artifact `alto-browser-reports`（GPU lifecycle の手動実行は `gpu-lifecycle-<OS>`）、ローカルは gitignore した `verification/out/`。以下のかつてのファイル名は、この変更より前の git 履歴にある。

2026-10-24。[設計](../design/COLD-RENDERING-DESIGN.md)の通常ラスタ向けP1/P2とC2/C3計測を実装した。可視ジョブの最大同期ステップを短縮する変更であり、コールド完全描画250 msは未達。派生OPFSキャッシュ、M5機能のジョブ化、C1/C4/C5の起動・検証・復旧内部の詳細時刻は未実装。

## 実装

- 先読み専用だった計画器を`TileJob`として可視描画にも共用。既存のソート済みsceneタイル列を二分探索し、可視タイルごとの全`BTreeMap`再構築を避ける。通常ラスタ、合成、孤立グループを対象にし、マスク・効果・調整・テキスト・メッシュ・ストローク中は既存の直接合成に戻す。
- 可視集合を保護して中央から処理する。完成済み子4枚が揃えば既存の縮小を使い、揃わなければ計画、検証済み入力の取得・転送、スロット再解決、合成提出を別ステップで進める。入力準備の途中で結果を公開しない。編集による無効化で進行中ジョブを破棄し、viewport変更では必要集合に残るジョブを継続する。
- 1ステップは約1.8 msを目安に処理し、Workerのイベントループへ戻る。単一のタイル取得・転送等が遅い環境では超過し得る。未完成フレームを自動継続し、メッセージ処理の機会を`MessageChannel`で作る。入力準備だけで画素が変わらないステップは同一画面のGPU提出を省く。
- 入力準備では、ジョブの全入力のうち既にアトラスにあるものを1ステップに1回保護する。アップロードごとのprefix集合再作成を除き、同じジョブが後で使う入力の退避を防ぐ。GPU配列容量は増やしていない。
- 内部診断要求`resetRenderCache {cpu}`はGPU入力・出力キャッシュを捨て、`cpu:true`ならデコード済みCPUキャッシュも捨てる。圧縮済み正本は残す。診断中は先読みを止め、`enablePrefetch`で再開する。`setVisibleTileJobs {enabled}`で同期参照経路に切り替えられる。これらは安定版SDKメソッドには追加していない。
- `maxStepMs`、`steps`、`uniqueInputs`（出力タイルごとの固有入力数の合計）、`atlasEvictions`を診断プロファイルに追加。リセット後の自動tickも集計し、`waitFrame`が返す`maxStepMs`は合計でなく最大値にする。

## 比較条件

Chrome 155、Apple Metal、3840×2160・半透明ラスタ20層、1093×827 CSS viewport、DPR 2、物理倍率0.5。C2/C3の実キャンバスは1566×1278、通常B1–B7では1566×1284となる。ログ表示によるレイアウト変化があるため、各JSONの`canvasPixels`を正とし、以前の1566×1284のコールド値と同条件と扱わない。

各新規ブラウザプロファイルで同期参照C2/C3、新経路C2/C3、B1–B7の順に実行する。同じWASMで可視ジョブだけ切り替えるため、同期参照も中央優先とWorkerの新しいyieldを使う。元の配布版全体との比較ではない。C2はCPUキャッシュを保持するが全入力がwarmという意味ではなく、各試行前のキャッシュ履歴にも差がある。公平なコールド比較はCPU/GPU双方を破棄するC3を中心に読む。計時は要求から全可視タイルの描画と`gpuIdle()`完了まで。未完成プレビューを完成と扱わない。

標準B1の前に再度GPUキャッシュを空にしてから先読みを再開し、コールド試験で用意した親がウォームB2へ残らないようにした。通常の120フレーム連続パン後、待機を挟まずB2を測る。

## 検証と残る課題

Rustテスト94件、native/wasm32 Clippy（警告をエラー扱い）、SDKビルド、ストレージWorkerのNodeテスト7件を通過。SwiftShaderは機能23、M5 8、M6 10、M7 9、M8 6の合計56件で失敗0。直接同期合成とコールド可視ジョブの画素一致を512×512全面、300×300透明端、回転、変形、Multiply、半透明孤立グループ、編集とUndoで確認した。256枚の表示キャッシュを埋めてから中断可能な可視ジョブで置換する試験も追加し、退避後のスロット索引を未使用に戻す修正を確認した。既存の保存障害・破損復旧試験も含む。SwiftShaderを性能判定には使っていない。

新経路では`commandsMs`が計画のみを計上する。同期参照の`commandsMs`には取得・転送も含まれるため、列の減少をそのまま高速化率と呼ばない。取得と転送は引き続き大きい。可視ジョブの重複除去は出力タイル内で行い、フレーム全体の共有バッチは未実装。完全描画の大幅な短縮が必要なら、今後は元タイルのデコード・検証・転送量を減らす設計が必要となる。

派生ディスクキャッシュは初めて表示する文書に効かず、GPU読戻し・OPFS保存・再検証の費用も未測定なので今回有効化していない。B7の完全描画は既存目標内にある。M5を含む文書の同期停止時間は今回の改善範囲外。別GPU、長時間編集後、実制作文書での性能は未検証。

開発中の別試行でB7 2062.3 msの失敗も観測した。記録（`cold-render-trial-outlier-2026-10-24.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）を残す。この試行時はビルドとSwiftShader検証が並行していた。以下の正式比較はそれらの終了後に実行した5回すべてであり、結果の良い回だけを抽出していない。

## 最終配布WASMとMetal 5回比較

配布WASM SHA-256: `6cc46a2f31b9386310e54390fb650291f7ad803864dcacb0f6a6af402dd8b86d`。PlaygroundとSDKへ同一バイト列を配置し、`build.json`と照合済み。

| 回 | C3 同期参照 | C3 新経路 | 同期参照の最長ステップ | 新経路の最長ステップ | B2 | B7 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1（`cold-render-metal-run1-2026-10-24.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`） | 396.4 ms | 453.5 ms | 13.5 ms | 2.1 ms | 2.9 ms | 1320.4 ms |
| 2（`cold-render-metal-run2-2026-10-24.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`） | 416.9 ms | 466.0 ms | 14.8 ms | 2.1 ms | 2.5 ms | 1408.4 ms |
| 3（`cold-render-metal-run3-2026-10-24.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`） | 457.4 ms | 411.9 ms | 16.4 ms | 2.1 ms | 2.8 ms | 1278.7 ms |
| 4（`cold-render-metal-run4-2026-10-24.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`） | 406.0 ms | 413.7 ms | 14.3 ms | 2.1 ms | 2.7 ms | 1299.6 ms |
| 5（`cold-render-metal-run5-2026-10-24.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`） | 404.2 ms | 408.1 ms | 13.4 ms | 2.1 ms | 3.1 ms | 1275.6 ms |

C3中央値は同期参照406.0 ms、新経路413.7 ms。完全描画時間の明確な短縮は確認できず、主な改善は長い同期処理を分割した点にある。250 msは未達。`maxStepMs`は合成ジョブ1ステップの時間で、Workerのtick全体や入力メッセージ応答時間の厳密な上限ではない。

- B1 GPU-complete frame p95 ms: 3.2–3.6 ms（中央値3.4 ms）。
- B2 zoom ms: 2.5–3.1 ms（中央値2.8 ms）。
- B3 continuous input-to-present GPU-complete p95 ms: 11.2–20.2 ms（中央値16.7 ms）。
- B7 cached Worker restart + first draw ms: 1275.6–1408.4 ms（中央値1299.6 ms）。

5回とも既存B1–B7の基準を満たし、ズーム前の親は35/35。GPU計上値484.75/512 MiB、WASMは各JSONのB6を参照。コールドC2/C3は診断値であり、JSONの`pass:true`は試験が完了した意味で、250 ms達成を意味しない。

SwiftShader集計（`cold-render-swiftshader-summary-2026-10-24.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。

CIのChrome 152では、参照画像の取得前に別倍率の自動描画が入ることで、参照側だけ完成子タイルからの縮小を使う試験の競合を検出した。参照側も対象倍率を指定してからGPUキャッシュを破棄する手順へ修正し、変形端の縮小と直接サンプリングを混同しないようにした。
