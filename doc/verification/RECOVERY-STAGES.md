# 復旧の段階別計時（PERF-5）

Apple M5、Chrome 157.0.8103.79、WebGPU Metal で確認した。GPU の watchdog は有効。比較元は `248d2cf`。各行で復旧を3回測り、時間は ms で記す。

`unstable(engine).recoveryStages()` は、現在の Worker で最後に成功したジャーナル復旧を返す。初回描画が GPU で完了すると `firstFrame` に時間が入る。描画を明示的に待つ場合は、`await unstable(engine).settle()` の後に読む。

`load` は採用した候補をストレージから読み込む時間で、Storage Worker のチェックポイント検証を含む。`verify` は Rust でパッケージを検査する時間。復旧全体には、Worker の起動や候補の再試行など、8段階の表に含まれない時間もある。

## Fixture 別の復旧

| Fixture | 変更前の各試行 | 変更後の各試行 | 中央値（変更前 → 変更後） |
|---|---|---|---:|
| `4k-20` | 2432.7、2411.3、2443.2 | 2529.0、2433.5、2483.9 | 2432.7 → 2483.9 |
| `fx` | 3034.0、3049.4、3053.7 | 3022.7、3099.4、3039.6 | 3049.4 → 3039.6 |

変更後の段階別中央値。

| Fixture | candidates | load | verify | restore | register | replay | activate | firstFrame |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `4k-20` | 0.2 | 7.9 | 9.0 | 228.7 | 15.6 | 27.5 | 0.9 | 1982.8 |
| `fx` | 0.2 | 71.6 | 5.7 | 83.9 | 119.1 | 13.4 | 1.0 | 963.3 |

6試行すべてで順序と非負値を確認し、段階の合計を復旧全体の時間と照合した。

## B7 の比較

他のビルド・試験が終わってから、変更前 → 変更後の順で3回ずつ測った。

| 版 | 各試行 | 中央値 | 最小–最大 |
|---|---|---:|---:|
| 変更前 | 2518.6、2574.5、2540.7 | 2540.7 | 2518.6–2574.5 |
| 変更後 | 2561.7、2555.2、2518.2 | 2555.2 | 2518.2–2561.7 |

測定範囲は重なり、中央値の差は 14.5 ms。B7 の目標値 1500 ms は変更前後とも未達。

## 測定コマンド

```sh
ALTO_PERF_FIXTURES=4k-20,fx \
ALTO_PERF_REPS='{"cold":1,"pan":1,"brush":1,"commit":1,"export":1,"save":1,"open":1,"recover":3}' \
ALTO_BROWSER_REPORT_DIR=/tmp/alto-recovery-perf \
  node scripts/verify-browser.mjs --suite=perf

ALTO_BROWSER_REPORT_DIR=/tmp/alto-recovery-b7 \
  node scripts/verify-browser.mjs --suite=benchmark
```

計測版では `perf.json` の `recover` 行と `browser-diagnostics.json` の `recovery` に、各試行の段階別時間を保存する。

## 親タイルの GPU 計時

`await unstable(engine).setGpuTimestampProfiling(true)` で計時を有効にする。復旧時に Worker を再起動しても設定は継続する。`gpuTileTimestamps()` は読み戻し済みのサンプルを取り出し、未対応のアダプターでは `null` を返す。計測を無効にすると、処理中のサンプルは破棄される。

各サンプルの `x`、`y`、`level` は親タイルの座標とレベル。`ns` はダウンサンプリングの compute pass の経過時間で、復旧全体や CPU 側の経過時間とは別の測定値。読み戻し枠は8件で、枠が埋まっている間は計時を省く。実測値はブラウザ側の丸めを受ける場合がある。[Chrome の GPU 計時に関する資料](https://developer.chrome.com/docs/web-platform/webgpu/developer-features?hl=en#disable_timestamp_queries_quantization)

`ALTO_BROWSER_GPU_TIMESTAMPS=1` を付けて perf を実行すると、対応環境の `recover` 行と `browser-diagnostics.json` に `gpuTileSamples` が出る。

Chrome 157.0.8103.79 の Metal で、`4k-20` と `fx` の復旧を各1回計測した。どちらもレベル1と2の親タイルが記録された。

| Fixture | 取得したサンプル数 | 最小–最大（ns） |
|---|---:|---:|
| `4k-20` | 8 | 10459–123331 |
| `fx` | 8 | 10166–18790 |

Metal と SwiftShader の組み込み試験で、計時を有効にした状態から Worker を再起動し、サンプル取得と復旧前後の画素一致を確認した。Chrome の `WebGPUService` で `timestamp-query` を非公開にした条件では、API が `null` を返し、機能試験57件が通過した。
