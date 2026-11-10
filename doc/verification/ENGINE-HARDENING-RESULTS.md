# エンジンの修正・検証：結果

計画：[2026-11-09-engine-hardening-plan.md](../design/2026-11-09-engine-hardening-plan.md)。

| 段階 | 状態 | 実行コミット | 環境 | 記録・PR |
|---|---|---|---|---|
| A | 成功 | `0e6f491` | 下記 | [A](#a基準となる検証) |
| B | 成功 | `1b5ed04` | 下記 | [B](#bcpu合成経路の削除) |
| C | 成功 | 下記 | 下記 | [C](#c書き出しの契約) |
| D | 実行中（1/7行） | 下記 | 下記 | [D](#d保存復旧の障害試験) |
| E | 未着手 | | | |
| F | 未着手 | | | |

## 環境

macOS 27.2、Apple M5（Metal 4）、rustc 1.99.1、Node v22.23.2、Chrome 155.0.8059.84（headless、Graphiteは既定のバックエンド）。

## A：基準となる検証

2026-11-09、`0e6f491` で実行。作業ツリーの変更は文書だけ。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `sh scripts/check.sh native` | 0 | fmt・clippy成功。Rustテスト28群・337件成功、失敗・無視0 |
| `sh scripts/check.sh wasm-fast` | 0 | WASMの同梱コピーが一致。SDKのビルド・API検査・単体テスト成功 |
| `node scripts/verify-browser.mjs --functional` | 0 | 8群・284件成功（下表）。予期しないGPUプロセスのクラッシュ0件 |

| ブラウザ試験 | 件数 | 結果 |
|---|---|---|
| image-versions | 13 | 成功 |
| functional | 37 | 成功 |
| m5 | 9 | 成功 |
| m6 | 10 | 成功 |
| m7 | 8 | 成功 |
| m8 | 6 | 成功 |
| embed | 37 | 成功 |
| parity | 164 | 成功 |

前回のレビューで中断された「GPU等のテスト群」を含め、全体が中断なしで完走した。既存の失敗はなかった。

## B：CPU合成経路の削除

`crates/alto-engine/src/edit.rs` を削除し、`Engine::export_rgba` と `Engine::sample_pixel` を取り除いた。

- 削除の根拠：workspace・SDK・WASMのいずれにも呼び出し元がない。Rustクレートは未リリースで互換性の約束がない（`RELEASING.md`）。SDKの書き出しはGPU経路、スポイトは `Engine::pick_color` を使い、どちらもこのコードに依存しない。
- これで解消した欠陥：大きな平行移動を `as i32` で黙って飽和させていたこと、親子の累積と画素−オフセットをi32で計算していたこと（debugでpanic、releaseで折り返し）、タイル形式の検査がない画素読み取りの重複。
- `edit.rs` にあったテスト `overlay_reaches_the_renderer_and_levels_bump_the_format` はこのメソッドを使っておらず、`scene_tests.rs` へ内容を変えずに移した。
- 描画の独立したCPU参照（`crates/alto-gpu/src/cpu_composite/`、`tests/e2e/cpu-composite.js`）は残した。

検証（`1b5ed04`）：

| コマンド | 終了コード | 結果 |
|---|---|---|
| `sh scripts/check.sh native` | 0 | fmt・clippy成功。Rustテスト28群・337件成功（移したテストを含む。件数はAと同じ） |
| `sh scripts/check.sh wasm-fast` | 0 | SDKのビルド・API検査・単体テスト成功。WASMの同梱コピーは一致。本体のSHA-256はAの `cb39675d…` から `c3ff1199…` に変わった（コード削除による想定どおりの変化。基準ハッシュは更新していない） |

ブラウザ試験はBでは再実行していない。変更は呼び出し元のないRustメソッドの削除で、WASMとSDKの公開APIは変わらない（`api:check` 成功）。

opencode（deepseek-v4.1-flash-free）のレビューは3点を挙げ、いずれも修正不要と判断した。import不足の指摘は誤り（`lib.rs` のimportが `use super::*` で届き、テストは成功）。公開APIの削除は未リリースのため問題ない。書き出し時のガードの消失は意図どおり。

過去の設計文書（`2026-10-24-embedding-api.md` など）と [FOLLOWUP-STAGES-2026-11-03](FOLLOWUP-STAGES-2026-11-03.md) に残る `export_rgba` / `sample_pixel` / `edit.rs` への言及は、当時の記録としてそのままにした。

## C：書き出しの契約

契約は [EXPORT-CONTRACT.md](../design/EXPORT-CONTRACT.md) に書いた。

見つけた欠陥と修正：

| 欠陥 | 再現 | 修正 |
|---|---|---|
| PNGの半透明の画素が、表示タイルから最大1ずれる | 512×256の半透明グラデーションで3,584画素。canvasが `putImageData` の画素をpremultipliedで持ち、`convertToBlob` で戻すときに丸めが2回入る | 縮小なしのPNGはSDKが自前で符号化する（`png.ts`）。修正前のworkerでは新しい試験が失敗し、修正後は全画素が一致することを確認した |
| `region` にNaNを渡すと `INTERNAL`（`OffscreenCanvas` の構築エラー）になる | `exportImage({ region: { x0: NaN, … } })` | `document.ts` で `INVALID_ARGUMENT`、`reason: "region"` にする。`ErrorDetails.reason` に `"region"` を足した |

計画で「合成結果のスポイトは別計画」としていたが、`image.pickComposite` が既にあった。書き出しと同じ規則でstraightに戻すことを確かめ、試験に入れた。

検証は下の「Cの検証」にまとめる。

### Cの検証

2026-11-09、同じ環境（Chrome 155.0.8059.84、Apple M5）で実行。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `sh scripts/check.sh wasm-fast` | 0 | SDKのビルド・API検査成功。単体テスト53件成功（`png.test.js` の2件を含む） |
| `node scripts/verify-browser.mjs --functional` | 0 | 8群・286件成功。embedはAの37件に新しい2件を足した39件。予期しないGPUプロセスのクラッシュ0件 |

Rustのコードは変えていないので、`check.sh native` は再実行していない。

opencode（deepseek-v4.1-flash-free）のレビューの2点：

- 1/2倍などで、範囲の終点が段の画素の途中にあっても完全一致の経路に入る：正しい。ただし修正前からの挙動で、奇数幅の文書を縮小したときに段の画素をそのまま出すためのもの。コードは変えず、契約とdocstringを実際の条件に合わせた。
- `x0 = +Infinity` で負の幅になる：誤り。`x1` は文書幅に収められ、既存の `x1 <= x0` の検査で `INVALID_ARGUMENT` になる。

PR #91の最初のCIで、新しい一致試験がSwiftShaderの描画の監視（`GPU_LOST: the previous frame did not finish on the GPU`）に当たって失敗した。文書を600×300から300×160に小さくして解消した（`4362d83`。CIの全項目が成功）。

ただしこれは解消ではなかった。PR #92のCIで同じ試験が同じ `GPU_LOST` で再び失敗した。SwiftShaderで単独に実行すると手元でも再現した。描画の時間を記録したところ、分離グループ・ベクター・調整レイヤーがあると文書全体がM5の合成器を通り、17合成モードの層を含む1フレームのGPU処理が3.5〜5秒かかっていた。監視は50 msの待ちを40回まで、約2秒で打ち切るので、GPUは失われていないのに `GPU_LOST` になる。試験は文書を2つに分けて直した（合成モードはタイルの合成器で約20 ms、M5の文書は2フレーム目で約0.4 s、待ちの連続は最大7回）。分けた後もSwiftShaderとMetalでembed 39件・crash 6件が成功し、canvas経由の符号化に戻すと1つ目の文書で940画素が1ずれて失敗することを確かめた。遅いGPUで重い1フレームを喪失と判定する監視の動作そのものは、この計画の範囲外として残す。

## D：保存・復旧の障害試験

計画の表を1行ずつ進める。

| 行 | 状態 | 記録 |
|---|---|---|
| 未確定ストローク、確定後・durable前、durable後 | 成功 | [下記](#d-1ストロークの3時点) |
| ジャーナル追記中 | 未着手 | |
| チェックポイント切替中 | 未着手 | |
| `.altoproj` 保存のmanifest確定前後 | 未着手 | |
| OPFSのGC削除中 | 未着手 | |
| 版のマージ確定中 | 未着手 | |
| Worker停止、GPU喪失 | 未着手 | |

### D-1：ストロークの3時点

仕組み：

- `tests/e2e/crash-tests.js` はNode側で動き、CDPでタブを開いて終わらせる。ページ側は `tests/e2e/crash-page.html`。`scripts/verify-browser.mjs` が機能試験の後に実行する（`--suite=crash` で単独、`ALTO_CRASH_REPEAT` で反復回数）。
- タブの終わらせ方は2つ。`Page.crash` はレンダラーを落とし、unloadを経ない。`Target.closeTarget` は利用者がタブを閉じるのと同じで、`pagehide` が走る。
- 待機点：SDKの `testing` 入口に `holdStorage(hold)` と `storageHeld()` を足した。Workerの書き込みループが、次のジャーナル追記またはチェックポイントの直前で止まる。編集はメモリに適用され、`durable()` は待つ。本番の公開APIは変えていない。
- 判定：同じオリジン（同じOPFS）の新しいタブで同じnamespaceを開き、`recovery.restore()` の結果を、snapshot hash、`stateHash`（内容と版の先頭）、層のタイルマップ、版履歴（DAG、全ブランチ）で比べる。復元がエラーなく終わっただけでは合格にしない。

| 時点 | 作る状態 | 期待 |
|---|---|---|
| `stroke-open` | 版を1つ作りdurableにした後、ストロークを描き始め、終えずに入力を送る | 直前のdurableな状態 |
| `applied` | 書き込みを止め、ストロークと層名の変更を適用する。止まった書き込みが追記であることを確かめる | 直前のdurableな状態。適用だけされた編集は戻らない |
| `durable` | 同じ編集の後に `durable()` が解決する | 編集後の状態 |

`revision` は比べない。セッション内の編集数で、パッケージを読むたびに0から数え直す（`crates/alto-doc/src/document.rs` の読み込み）。チェックポイントもパッケージなので、復旧後の値は契約に含まれない。最初の実行では比較に含めていて、6件すべてがこの差だけで失敗した。ほかの項目はすべて一致していた。

試験が失敗を検出できることも確かめた。`applied` で、止めた書き込みをタブを終わらせる前に流すと、2件が「durableにならなかった編集を持っている」として失敗した。

欠陥は見つからなかった。

検証：

2026-11-10、Aと同じ環境（Chrome 155.0.8059.84、Apple M5）で実行。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `sh scripts/check.sh wasm-fast` | 0 | SDKのビルド・API検査成功。単体テスト53件成功 |
| `ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`（Metal） | 0 | 3時点×2方法×10回の60件成功。1件184〜492 ms |
| 同上、`ALTO_BROWSER_GPU=swiftshader` | 0 | 60件成功。1件3.2〜3.5 s |
| `ALTO_BROWSER_GPU=swiftshader node scripts/verify-browser.mjs --functional` | 0 | 9群・292件成功（Cの286件にcrashの6件）。予期しないGPUプロセスのクラッシュ0件 |

各待機点で、2つの終わらせ方それぞれ10回連続で成功した。CIは反復1回（6件、SwiftShaderで約20秒）で実行する。Rustのコードは変えていないので、`check.sh native` は再実行していない。

opencode（deepseek-v4.1-flash-free）のレビューは11点を挙げた。直したのは1点：`storageHeld()` の待ちが残ったまま保留を解くと、その待ちが解決せず、次の保留で別の書き込みの種類を受け取る。解除時に `CONFLICT` で拒否するようにした（試験専用の入口）。ほかは誤りか修正不要と判断した。`durable()` は `pendingBytes` ではなく書き込みの通し番号で待つので、保留中は解決しない。層のIDは文字列で、タブをまたいでも同じ文書の同じ層を指す。期待値の取得順・JSONの比較順・ロックの解放は、Metal・SwiftShaderの各60件で問題が出ていない。`--suite` で選ばない限り走らない動作と、ほかの群が失敗したら走らない動作は、既存の群と同じにした。
