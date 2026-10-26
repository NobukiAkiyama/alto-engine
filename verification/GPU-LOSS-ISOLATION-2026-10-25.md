# GPU 喪失の追加切り分け（2026-10-25）


生の検証JSONはこのディレクトリに置かない。新しい実行は CI の artifact `alto-browser-reports`（GPU lifecycle の手動実行は `gpu-lifecycle-<OS>`）、ローカルは gitignore した `verification/out/`。以下のかつてのファイル名は、この変更より前の git 履歴にある。

## 結論

**元の macOS CI と同じ SharedImage / present の異常を再現した。さらに、起動直後・最初の機能テストより前にも発生し、自動復旧の後で 93 件すべてが成功するケースを確認した。**

したがって、M5 のブラシ、保存復旧、Worker の再起動が発生条件として必須とは言えない。また、機能テストの合格だけでは「予期しない GPU 喪失が一度もなかった」とは確認できない。

**mailbox が消える最下層の原因、対応する Chromium の不具合番号、Alto の特定のコード行までは特定できていない。** 単純な Canvas 操作・資源の確保だけでは macOS の障害を再現できなかった。Alto がブラウザの資源寿命の問題を誘発している可能性も残る。

これは追加調査の結果であり、GPU 喪失を修正したという報告ではない。前回の証拠は [GPU-LOSS-INVESTIGATION-2026-10-25.md](GPU-LOSS-INVESTIGATION-2026-10-25.md)。今回の数値・プロセス情報・エラー行の抜粋は `gpu-loss-isolation-ci-2026-10-25.json`（CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。

## 1. 起動中の喪失が、合格結果の裏で起きていた

[40170361518](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40170361518) の macOS の Chrome stderr は、次の順序だった。時刻は CI の UTC。

1. `06:47:56.872`：空のシーンの `present`。
2. `06:47:57.158`：存在しない mailbox から `ProduceSkia`。
3. `06:47:57.169`：`kDissociateMailboxForPresent` のエラー 4。
4. `06:47:57.210`：初めての機能テストの `RUN`。
5. `06:47:57.313`：`A valid external Instance reference no longer exists.`。
6. 以後、機能 32・M5 8・M6 10・M7 9・M8 6・組み込み 28、計 93 件が成功。

GPU PID は `1198` のままで、クラッシュ数も 0。強制 GPU 喪失の試験は後で実行されており、この起動時の異常とは別。

この実行と、同条件の [40170362968](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40170362968)・[40170364667](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40170364667) の赤い CI 結果は、**トレースを終了しても 10 秒以内に stream が返らなかったことによる診断収集の失敗**。3 本とも機能 93 件は合格している。3 本の CI 失敗を GPU 喪失 3 回とは数えない。

トレースの終了待ちは 45 秒に変更し、収集に失敗すれば独立した `gpu-trace` の失敗として残す。エンジンの初期化や編集要求のタイムアウトを延ばしたものではない。

## 2. エンジンを外した比較

新しい再現用ファイルは `tests/diagnostics/gpu-lifecycle*`。既定は JavaScript の WebGPU だけを使い、WASM・Rust・SDK・OPFS・デコード Worker を使わない。

| macOS の条件 | 実行内容 | 結果 |
|---|---|---|
| 資源確保なしの raw WebGPU | 通常表示、同サイズ・異なるサイズ・連続リサイズ、Worker の強制終了と通常終了、デバイス再作成と unconfigure の比較、9 条件 × 120 回 | 全条件成功 |
| 192 MiB のテクスチャ確保を追加 | 同じ 9 条件 × 120 回 | 全条件成功 |
| GPU 完了を毎回待たず、計算と表示を投入しながら resize | 120 回 | 成功 |
| 同様の投入中に 10 回ごとに Worker を強制終了・差し替え | 120 回 | 成功 |
| 配布 WASM の Runtime だけを追加 | 空の文書、描画検証と内容タイルの読み戻し、9 条件 × 20 回 | 全条件成功 |

raw WebGPU の通常条件は、実際の surface を緑で消去し、GPU buffer に 1 画素をコピーして `[0,255,0,255]` を検査する。処理を投入する条件では最後の読み戻しで確認する。

192 MiB は確保要求の大きさであり、実際に常駐したメモリの計測値ではない。WASM の試験も空の文書であり、マスク・筆描・保存や SDK のスケジューラまで網羅したものではない。

主な実行は [raw 0 MiB](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40168974259)、[raw 192 MiB](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40169398207)、[resize 中の投入](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40170286940)、[終了中の投入](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40170288390)、[WASM Runtime](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40169827605)。一部の実行全体は、併設した実験的 Linux ジョブの失敗で赤になっている。上表は macOS ジョブの結果。

最初の切り分けコードには adapter の再利用や限界値の指定の誤りがあり、修正前の失敗は上表に含めていない。また、WASM の 120 回の Worker 差し替えは試験全体の 180 秒を超えたため、その結果を GPU 障害の再現とは扱わず、20 回で確認し直した。

## 3. 本体では再現し、表示バックエンドの変更は未確定

[40169401525](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40169401525) の macOS は機能 32 件の後、M5 の最初のテストで喪失した。先行する異常は前回と同じ `ProduceSkia` → `DissociateMailboxForPresent` → 読み戻し失敗で、GPU PID `1276` は継続していた。

一方、標準の `GraphiteDawnMetal` で 93 件に成功する実行もある。

表示側も `--enable-skia-graphite --skia-graphite-dawn-backend=swiftshader` で比較したところ、実際に `GraphiteDawnVulkan` になり、[40168975912](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40168975912) では 93 件と Vite サンプル 2 本、[40169769107](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40169769107) の macOS では 93 件と GPU トレース保存が成功した。

**この少数の成功から、Metal と SwiftShader の受け渡しが原因、またはこのフラグが対策、とまでは結論できない。** 標準の表示経路でも合格するため、既定の描画設定を変更する根拠には足りない。

## 4. 起動だけの比較を追加

`node scripts/verify-browser.mjs --startup-only` は実際の Playground / SDK を新しいプロファイルで起動し、描画完了と snapshot を確認する。その間に missing mailbox が出れば、自動復旧後でも失敗とする。故障注入の試験は行わない。

[標準表示の 5 回](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40171935538) と [表示側 SwiftShader の 5 回](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40171937105) は、いずれも成功した。これは 10 回の観測であり、起動障害が解消した証明ではない。

このモードは診断用の手動 CI に追加した。通常の correctness suite の 93 件という集計と、予期しない喪失のない起動の確認を区別して記録できる。

## 5. Linux で同じ文言が出ても、原因は一致しなかった

Linux の raw JavaScript の最初の表示でも同じ external Instance の文言が出たが、その前には `Could not find SharedImageBackingFactory` があり、Skia は `None` だった。これは表示用 SharedImage の生成経路が使えない構成。

Ubuntu の AppArmor による Chrome 起動制限は、当該 CI の実行ファイルだけを許可して切り分けた。その後 Graphite を明示的に有効にすると別の GPU プロセスクラッシュが起きた。この経路も、GPU プロセスが生存している元の macOS の障害とは同一視できない。

従って、今回 Linux を安定した比較対照として確立できていない。手動の診断ジョブは既定を macOS とし、Linux は明示指定する実験用の選択肢にした。

[Chromium の Headless GPU の説明](https://chromium.googlesource.com/chromium/src/+/HEAD/docs/gpu/using-gpu-hardware-in-headless-chrome.md)、[AppArmor の説明](https://chromium.googlesource.com/chromium/src/+/main/docs/security/apparmor-userns-restrictions.md)、[実際の SharedImage 生成コード](https://chromium.googlesource.com/chromium/src/+/refs/tags/156.0.8078.47/third_party/blink/renderer/platform/graphics/gpu/webgpu_swap_buffer_provider.cc) とも照合した。

## 残る調査

次に必要なのは、**異常な起動の際に、同じ mailbox の生成・利用・破棄と Worker / Canvas を対応付けること**。通常配布 Chrome のログには消えた mailbox の ID がなく、取得できた成功時のトレースだけでは因果関係を決められなかった。Chromium の SharedImage 管理側に mailbox ID とクライアント ID を残す計測が必要になる。

現段階でエンジンの保存方式やタイル設計を変更する根拠は得られていない。調査によって分かったのは、表示用資源の異常が起動時にも起き、機能テストの合格と共存し得ること、および単純な操作に分解した試験では再現しないこと。特定のコード行を原因とした修正案には、まだ証拠が不足している。

## 手元の整合性確認

追加した JavaScript 3 ファイルの構文検査、SHA256SUMS の 365 項目、配布 WASM 2 箇所のビルドメタデータ、空白差分検査は成功した。WASM の SHA-256 は引き続き `15e82969ba684c500a7b27feaf8ee909d945b4a20ab994724fe9db9594197f2d`。これらは診断コードと成果物の整合性の確認であり、間欠障害の解消の判定ではない。

調査コード `5b2faab` の [通常 CI 40171935959](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40171935959) も成功した。`scripts/check.sh`、ブラウザ 93 件、Vite サンプル 2 本は成功した。しかし、この実行でも `two damaged journals freeze editing until an external file opens` の後、`07:16:13.528` に missing mailbox、`07:16:13.536` に present のエラー、`07:16:13.704` に external Instance の喪失文言が出た。その後 `07:16:13.735` に機能 32 件が合格し、残りも完走した。**最新の緑の CI にも GPU 喪失が含まれていた**ことを、修正完了との誤解が生じないよう記録する。
