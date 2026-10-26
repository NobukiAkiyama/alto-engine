# GPU 喪失の呼び出し記録による調査（2026-10-25）

## 判定

**表示用 SharedImage の取り込み失敗 → 無効な表示テクスチャ → present のコマンドエラー → デバイス喪失、という故障経路を、実際の SDK の呼び出し記録付きで確認した。mailbox が登録前なのか、早く破棄されたのかを決める根本原因の特定には、まだ至っていない。**

今回の進展は、障害直前のデバイス・Canvas・configure・寸法設定・取得・投入・明示的破棄を対応付けられたこと。初回起動の再現では、最初のデバイスと最初の Canvas のまま発生しており、**先行する Worker 差し替えやアプリからの device / texture の destroy はない**。ブラウザ内部の自動破棄は JavaScript の記録では観測できない。

[前回の切り分け](GPU-LOSS-ISOLATION-2026-10-25.md) と合わせて読む。今回の呼び出し記録、実行条件、プロセス、成果物のハッシュと結果は [構造化した証拠](gpu-loss-surface-ci-2026-10-25.json) に保存した。修正済みとする報告ではない。

## 1. 最初のデバイスでの再現

[CI 40175618230](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40175618230)、Chrome 156.0.8078.47、WebGPU は SwiftShader、表示側は GraphiteDawnMetal。

Worker の realm は `fa29c42f-b2d1-4d6a-99c0-b962abbdb5ab`。全 82 イベントが最初の異常まで保存された。

| UTC 時刻 | 観測 |
|---|---|
| 08:11:28.704 | 最初の device 2 が作られる |
| 08:11:28.705–706 | wgpu の 1×1 rgba16float 能力試験を configure / unconfigure |
| 08:11:28.707 | 本体の Canvas 6 / context 7 を 300×150、BGRA、RenderAttachment のみ（usage 16）、opaque で configure |
| 08:11:28.776 | 同じ Canvas を 970×412 に変更、configure |
| 08:11:30.803 | 同じ Canvas を 970×415 に変更、configure |
| 08:11:30.804–980 | 同じ context で取得 19–23 と queue への投入 36–40 |
| 08:11:31.052 | GPU サービスで最初の `non-existent mailbox` |
| 08:11:31.053–054 | 無効な Texture の CreateView。記録した validation も同じ内容 |
| 08:11:31.057 | `kDissociateMailboxForPresent` のエラー 4 |
| 08:11:31.230 | device 2 が reason unknown / external Instance の文言で喪失 |
| 08:11:31.269 | 最初の機能試験 RUN |

GPU PID は 1638 のまま、クラッシュ数は 0。明示的な `destroy-device` / `destroy-texture` は、この異常前の記録にはない。Canvas 6 の unconfigure もない。unconfigure されたのは能力試験の別 Canvas 5 だけ。

**93 件の試験は、以後の自動復旧を経てすべて通った。** この実行の緑は、喪失がなかったという意味ではない。

## 2. 別の Worker でも寸法変更の後に再現

[CI 40176072928](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40176072928) は、機能試験の `missing Worker script stops once and retries without corrupting the journal` の再試行後に失敗した。

新しい Worker 内の device 2 / Canvas 6 で、970×314 → 970×415 の寸法設定と configure が `08:19:09.320`、最初の missing mailbox が `08:19:09.472`、validation が `08:19:09.474`。この Worker 内にも、異常前の device / texture の明示的 destroy はない。全 41 イベントを保存した。

最初の再現には Worker 差し替え自体がないため、この二例から「Worker 終了が必要条件」とは言えない。寸法変更との時間的な関係は観測できたが、**リサイズだけが原因と証明したものでもない**。

## 3. 比較試験

| 条件 | 結果 |
|---|---|
| 通常操作も console に記録する SDK 計測 | 3 実行 × 93 件が成功、予期しない喪失なし |
| 異常まで console 出力を抑えた SDK 計測 | 40175618230 は起動時に喪失して後で 93 件成功。40175744288 は 93 件成功で喪失なし。40176072928 は機能 21 件まで成功して喪失。40186994727 は起動時に喪失して後で 93 件成功、追加した喪失検出だけが失敗 |
| 記録付きの新規起動 | 新しいプロファイル 5 回すべて成功 |
| 未描画の surface をそのまま present | raw WebGPU 120 回成功（40175839138） |
| 未描画の present の後で Worker / Canvas を差し替え | raw WebGPU 120 回成功（40175840909） |
| 記録した起動時の surface 呼び出し順を raw WebGPU で再実行 | 192 MiB の確保を追加し、20 世代成功（40176279218） |
| SDK のリサイズ直前に `await runtime.gpuIdle()` | 40176280677・40176322813・40186993317 は各 93 件成功、予期しない喪失なし |

console の多い条件と少ない条件で観測が違ったが、サンプル数が少なく、ログ出力が障害を抑えたという因果関係までは確定できない。

raw の起動再実行は、300×150 の初期寸法、970×412 / 415 の変更、rgba16float 能力試験、BGRA / usage 16、取得と投入の相対時刻を含む。Alto の GPU コマンド内容は緑の clear と空の submit に置き換えている。したがって、呼び出し順だけを再現した比較であり、本体と等価な負荷の再現ではない。

最初の再実行用コード（40176071106）は取得と submit の間で timer に yield してしまい、surface texture の自動失効による `Destroyed texture ... used in a submit` を発生させた。これは試験側の誤りで、元の missing mailbox の再現には数えない。同じ task で取得・投入するよう修正し、20 世代をやり直した。

静かな計測の通常条件は 4 実行中 3 実行で喪失、GPU 完了待ちの条件は 3 実行中 0 実行だった。これは GPU 処理と Canvas 再設定・表示用資源の同期を調べる根拠になる。ただし、少数の観測であり、待機による実行タイミングの変化とも区別できず、修正の証明ではない。`gpuIdle()` は提出済み GPU 処理の完了待ちであり、表示側の全資源の受け渡しまで完了した証明ではない。既定の動作や配布 Worker は変更していない。

## 4. Chromium のコードとの照合

対象は実際に使った `156.0.8078.47` のソース。

- [WebGPU decoder](https://chromium.googlesource.com/chromium/src/+/refs/tags/156.0.8078.47/gpu/command_buffer/service/webgpu_decoder_impl.cc)：CPU adapter では `AssociateMailboxUsingSkiaFallback` が `ProduceSkia` を使う。取り込み失敗時は `ErrorSharedImageRepresentationAndAccess` の無効テクスチャを渡す。`HandleDissociateMailboxForPresent` には未初期化の画像を Skia で消去し、失敗すれば `kInvalidArguments` を返す経路もある。
- [swap buffer provider](https://chromium.googlesource.com/chromium/src/+/refs/tags/156.0.8078.47/third_party/blink/renderer/platform/graphics/gpu/webgpu_swap_buffer_provider.cc)：SharedImage の pool と sync token を通じて生成・再利用・表示への受け渡しを行う。
- [SharedImage manager](https://chromium.googlesource.com/chromium/src/+/refs/tags/156.0.8078.47/gpu/command_buffer/service/shared_image/shared_image_manager.cc)：`ProduceSkia` の今回の文言は、mailbox が管理表に見つからない分岐。最後の参照がなくなった際の削除は `OnRepresentationDestroyed` にある。

**今回の invalid Texture に先行する mailbox 取り込みの異常は、Rust の Instance が先に落ちたという説明とは合わない。** ただし、ブラウザ側のこの分岐が通った事実だけで、Alto に誘発条件がない、あるいは特定の Chromium バグと同一、とまでは言えない。

## 5. 追加した診断と現在の限界

`tests/diagnostics/surface-recorder.js` は 1,024 イベントのリングに数値・文字列だけを保存する。native GPU オブジェクトをクロージャで保持しない。診断サーバーが配信時に注入し、配布 JS / WASM の内容は変更しない。

`ALTO_BROWSER_SURFACE_RECORDING=1` の実行では、予期しない validation / loss の dump が一度でも出れば、後で試験が成功しても `surface lifetime recording` を失敗にする。意図的に destroy したデバイスからの通知は dump 対象から除外する。通常 CI の既定に計測を強制したものではない。

`ALTO_BROWSER_RESIZE_BARRIER=1` は比較用で、配信する Worker の resize 前だけ GPU 完了待ちを挿入する。挿入先が一意に存在しなければ配信を拒否する。製品修正として採用していない。

残る決め手は、同一 mailbox の `Register`、`ProduceSkia`、`OnRepresentationDestroyed` と、生成 / 破棄要求の client ID・sync token を GPU サービス内で記録すること。今回の通常配布 Chrome のログにはその ID がないため、**未生成・早期破棄・別の mailbox の参照、のどれかを決められない**。JavaScript 側の記録を増やすだけでは、この管理表の遷移は見えない。

現時点で確定した故障箇所と、まだ確定していない根本原因を区別する必要がある。今回の変更は診断であり、間欠障害を解消した変更ではない。


## 手元への反映と検証

診断コードと本報告は主作業フォルダにも反映した。JavaScript の構文、配布 WASM 2 箇所のビルドメタデータ、診断変更の空白差分、SHA256SUMS を確認した。今回 Rust と配布 Worker / WASM は変更していない。WASM の SHA-256 は `15e82969ba684c500a7b27feaf8ee909d945b4a20ab994724fe9db9594197f2d` のまま。

最後の [通常条件 40186994727](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40186994727) は、機能 93 件すべてが成功したにもかかわらず、起動時の喪失を `surface lifetime recording` が検出し、CI 全体が赤になった。これは追加した診断判定の実例であり、GPU 障害の修正成功を示すものではない。
