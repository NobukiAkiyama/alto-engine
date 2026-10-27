# CI の間欠的な GPU 喪失：調査結果（2026-10-25）


生の検証JSONはこのディレクトリに置かない。新しい実行は CI の artifact `alto-browser-reports`（GPU lifecycle の手動実行は `gpu-lifecycle-<OS>`）、ローカルは gitignore した `verification/out/`。以下のかつてのファイル名は、この変更より前の git 履歴にある。

## 結論と限界

元の `GPU_LOST` を、ブラウザの版を固定した GitHub Actions でも再現した。**最初に観測された異常は、表示用 SharedImage の mailbox が存在しないことだった。無効な表示テクスチャ、present コマンドの失敗、その後のデバイス喪失という順序を確認した。**

これは失敗する経路の特定であり、mailbox が消える最下層の原因まで特定したという意味ではない。Chromium / Dawn の表示用資源の受け渡しと、Alto の Canvas 差し替え・Worker 終了との関係は、さらに最小再現で確認する必要がある。

GPU プロセスの PID は変わらず、`processCrashCount` は 0 のままだった。今回の再現を GPU プロセスのクラッシュや監視タイマーによる終了とは判定できない。Rust の `wgpu::Instance` が解放されたという根拠も得られていない。

**Chrome の版固定だけでは解消していない。監視タイマーの無効化も、CI 全体の失敗を解消しなかった。失敗を成功に変える再試行や、WebGPU 以外への描画の切り替えは追加していない。**

## 条件

- 調査用ブランチ：`codex/gpu-loss-ci`。土台は `35be75a`。直前の文書所有・復旧破棄の修正も含めた。
- エンジンの配布 WASM は同じものを使用した。SHA-256 は `15e82969ba684c500a7b27feaf8ee909d945b4a20ab994724fe9db9594197f2d`。
- Actions は `macos-15` / arm64。エンジンの WebGPU adapter は SwiftShader だったが、ブラウザの表示側は `GraphiteDawnMetal` と `Apple Paravirtual device` を使用していた。
- ローカルは Apple M5。Chrome for Testing 152.0.7977.82 と 156.0.8078.47 の双方で、既存 65 件と組み込み 28 件が成功した。両者とも GPU プロセスの PID は変わらなかった。
- 元の CI の通常 Chrome は 152.0.7977.83。比較に使用した Chrome for Testing は公開されている 152.0.7977.82 であり、同一パッチ版とは扱わない。

## 実際の CI 結果

同じ WASM を使っても結果が変わった。版を固定した比較には複数のランナーが使われており、下表だけから特定の Chrome 版の欠陥とは断定しない。

| 実行 | Chrome for Testing | GPU watchdog | 結果 |
|---|---|---|---|
| [40165033186](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40165033186) | 156.0.8078.47 | 有効 | 93 件と Vite の例 2 本が成功 |
| [40165091293](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40165091293) | 152.0.7977.82 | 有効 | Worker の init が 30 秒でタイムアウト |
| [40165329221](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40165329221) | 152.0.7977.82 | 無効 | 既存 65 件は成功、組み込みテスト中に init がタイムアウト |
| [40165562420](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40165562420) | 156.0.8078.47 | 有効 | 機能テスト中に SharedImage / present 異常から GPU 喪失 |
| [40165803331](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40165803331) | 156.0.8078.47 | 有効 | 機能 32 件の後、M5 中に同じ表示経路の異常から GPU 喪失 |
| [40165835977](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40165835977) | 152.0.7977.82 | 有効 | M7 中に init がタイムアウト。最終段階は `wasm-load` |
| [40166157495](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40166157495) | 156.0.8078.47 | 有効 | CI 全体が成功 |

数値・設定・プロセス情報の抜粋は `gpu-loss-ci-summary-2026-10-25.json`（CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。生のブラウザ記録と Chrome stderr は、各実行の `alto-browser-reports` artifact にある。

## GPU 喪失直前の順序

156 系の実行 40165562420 では、次の順序だった。

1. `SharedImageManager::ProduceSkia` が、存在しない mailbox からの画像取得を報告。
2. 表示用 `Texture` と `TextureView` が無効になり、`present` レンダーパスと command buffer が失敗。
3. `webgpu_decoder_impl` が `kDissociateMailboxForPresent` に対するエラー 4 を報告。
4. 未初期化の SharedImage を読み取ろうとしたことを compositor が報告。
5. `A valid external Instance reference no longer exists.` が届く。
6. タイルの読み戻しの `map_async` が失敗し、Alto が `GPU_LOST` を返す。

後の実行 40165803331 でも、SharedImage / present の同じ異常を観測した。失敗するテストは異なり、表示タイルの読み戻しとストロークの読み戻しの両方で表面化している。

## エラーメッセージの解釈

Chromium 152.0.7977.83 の [DEPS](https://chromium.googlesource.com/chromium/src/+/refs/tags/152.0.7977.83/DEPS) にある Dawn revision `ab8827bc57b176eeaa89f71324130c02d4d41145` のソースも確認した。

- [Dawn Device.cpp](https://dawn.googlesource.com/dawn/+/ab8827bc57b176eeaa89f71324130c02d4d41145/src/dawn/wire/client/Device.cpp) は、イベント処理の shutdown 時にもこの文言を設定する。
- [Dawn Client.cpp](https://dawn.googlesource.com/dawn/+/ab8827bc57b176eeaa89f71324130c02d4d41145/src/dawn/wire/client/Client.cpp) の `Disconnect()` はイベント管理を終了する。
- Chromium の [WebGPUImplementation](https://chromium.googlesource.com/chromium/src/+/main/gpu/command_buffer/client/webgpu_implementation.cc) は、GPU コンテキスト喪失時に Dawn の接続を終了する。
- wgpu 30.0.1 の [Instance の契約](https://docs.rs/wgpu/30.0.1/wgpu/struct.Instance.html) は、Instance 自体を生存させ続ける必要はないとしている。

したがって、この文言だけから Rust の `Instance` の寿命を一次原因とすることはできない。保持追加の後でも今回の異常は再現した。

## 別扱いにする初期化タイムアウト

初期化段階の診断を追加したところ、152 系の実行 40165835977 は `wasm-load` で止まっていた。この段階は `Runtime.create()` より前であり、GPU の作成待ちや OPFS の準備待ちと同一視しない。監視タイマーを無効にした別実行でも init タイムアウトが出たが、その実行には段階診断がまだなかったため、同じ待ちだったとは断定しない。

WASM の取得・コンパイル・インスタンス化の内訳や、CPU / メモリ圧迫の影響は未確定。今回はタイムアウト値を延ばして成功扱いにする変更をしていない。

## 追加した診断と CI の変更

- Chrome for Testing を正確な版で固定し、実際の起動版が異なれば失敗させる。以前の成功実行 40162539571 には、同一ジョブ内で Chrome 152 と 156 の両方が記録されていた。なぜ切り替わったかは未確定だが、この変動をなくした。
- WebGPU adapter が指定した SwiftShader であることを検査する。
- Chrome stderr を逐次保存し、CDP の GPU / プロセス情報、console / exception / crash イベント、実行引数、Worker / WASM のハッシュを artifact に残す。
- 失敗した機能テストの名前を JSON に残す。
- Worker 初期化の最終段階をタイムアウトの説明に残す。`wasm-load`、`runtime-create`、`decoder-start`、`journal-configure`、`journal-inspect` を区別する。
- 手動実行で Chrome の版と watchdog の条件を変えられるようにした。既定では watchdog は有効のまま。

Chrome for Testing が自動更新しないことは [Chrome の公式説明](https://developer.chrome.com/docs/automation-and-testing/chrome-for-testing) に基づく。

## 不採用にした試験と次の切り分け

ローカルで Skia Graphite の無効化や ANGLE SwiftShader への統一も試したが、当該 Mac の構成では WebGPU が `unavailable_software` となった。これを CI の対策として採用していない。

次に有効なのは、Worker 終了・Canvas 差し替え・resize と SharedImage の異常の関係を、小さな JavaScript / WebGPU の再現例で確認すること。同じ WASM の Linux / SwiftShader と macOS 仮想 GPU 上での比較も、表示系の切り分けになる。Alto がブラウザ側の競合を誘発している可能性は残るため、エンジン側に問題がないとは結論しない。

**調査用 CI が一度成功したことを、間欠障害の解消とは扱わない。現時点では、喪失に至る表示経路と、別の WASM 初期化待ちを特定し、再発時に証拠を残せる状態にした。**
