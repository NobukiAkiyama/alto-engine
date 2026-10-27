# DPR測定とWorker復旧の実装結果


生の検証JSONはこのディレクトリに置かない。新しい実行は CI の artifact `alto-browser-reports`（GPU lifecycle の手動実行は `gpu-lifecycle-<OS>`）、ローカルは gitignore した `verification/out/`。以下のかつてのファイル名は、この変更より前の git 履歴にある。

2026-10-22、Chrome 155.0.8059.84 / Apple Metal WebGPU、1093×827 CSS viewport、同じ新規Chromeプロファイル条件、3840×2160・半透明20層で測定。各値は**1回の測定**で、機種間比較や統計的な優劣は主張しない。B2はズーム要求からGPU完了まで。B7はWorker再起動から復旧後の初回完全描画まで。

| 条件 | B2 | 可視文書範囲 | B2転送 | B7 | 判定 |
| --- | ---: | ---: | ---: | ---: | --- |
| DPR 1、CSS上の倍率0.5 | 208.4 ms | 1566×1284 | 867回 / 227.3 MB | 1246.2 ms | B2/B7合格 |
| DPR 2、CSS上の倍率0.5 | 104.7 ms | 1566×1284 | 360回 / 94.4 MB | 1385.2 ms | B2/B7合格 |
| DPR 2、物理倍率0.5 | 428.8 ms | 3132×2568 | 1960回 / 513.8 MB | 1317.9 ms | B2未達、B7合格 |

生データ: DPR 1・同じ見た目（`dpr1-same-view-2026-10-22.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）、DPR 2・同じ見た目（`dpr2-same-view-2026-10-22.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）、DPR 2・同じ物理倍率（`dpr2-same-physical-zoom-2026-10-22.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。DPR 1/2で同じ見た目の条件は、CSSキャンバス783×639、物理キャンバス783×642 / 1566×1284、可視文書1566×1284で一致した。DPR 1のCSS寸法と物理寸法には3px差がある。B2の可視範囲は実際の物理キャンバス寸法から算出した。

DPR 2の同じ見た目が今回速いのは、物理ズームが1.0で、DPR 1の物理ズーム0.5と**異なるLOD・キャッシュ経路**を通るためでもある。高DPI一般の高速化を示すものではない。今回のDPR 2は直前の同条件測定64.7 ms / 180回の転送から104.7 ms / 360回に変動したため、単発値の順位は評価しない。同じ物理倍率0.5では、DPR 2は約4倍の文書面積を描き、転送が513.8 MBに増える。この低倍率条件は未解決のボトルネックとして残る。

## 実装と検証

- SDKに`Booting / Running / Recovering / Failed / Disposed`、Worker世代番号、単一の再起動Promise、`engineFailure`、`retryWorker()`を追加。`init`と`recover`は30秒で打ち切り、Workerのロード失敗を保存破損と分ける。復旧が完了するまで編集・保存要求を止める。
- `viewport.zoom`の既存の物理ピクセル単位を維持し、`getCssZoom()` / `setCssZoom()`と`effectiveDpr`を追加。DPRが変わった際はCSS上の倍率を維持する。Playgroundのfit/パンも同じ係数を使用する。
- PlaygroundにWorker障害表示と再試行ボタンを追加。再接続後に文書ハンドルを付け直す。404のWorkerを注入して停止・再試行・文書ハッシュと画素の一致を確認した。
- SwiftShaderのブラウザ機能試験はM4相当20件、M5 8件、M6 10件、M7 9件、M8 6件、計53件が合格。M4/復旧の生データ（`worker-resilience-functional-2026-10-22.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）、集計（`worker-resilience-summary-2026-10-22.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。二重再起動は1回のCanvas交換と同じPromiseに収束し、送信できないリクエストは保留キューに残らなかった。
- Rustのworkspaceテスト、fmt、Clippy、配布WASMのバイト一致、SDKビルドを確認した。[GitHub Actionsの実行39724391776](https://github.com/NobukiAkiyama/alto-engine/actions/runs/39724391776)はコミット`4dae1b5`で成功し、macOS arm64のビルド照合とSwiftShaderのブラウザ機能試験も通過した。

## 続けて扱う範囲

同じ見た目のDPR 2はB2目標250msを満たしたため、GPUキャッシュの先読み・退避規則、P5のバッチ転送、ステージング方式変更は今回採用しなかった。DPR 2・CSS倍率0.25の428.8msを改善するには、低倍率でのアップロード回数・可視タイル容量を対象に、画素一致とGPU 512 MiB予算を保った別の比較実装が必要。30秒タイムアウト、依存モジュール404、初期化中の例外、旧世代から遅延した応答の個別障害注入は未検証。
