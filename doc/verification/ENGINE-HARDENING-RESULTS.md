# エンジンの修正・検証：結果

計画：[2026-11-09-engine-hardening-plan.md](../design/2026-11-09-engine-hardening-plan.md)。

| 段階 | 状態 | 実行コミット | 環境 | 記録・PR |
|---|---|---|---|---|
| A | 成功 | `0e6f491` | 下記 | [A](#a基準となる検証) |
| B | 成功 | `1b5ed04` | 下記 | [B](#bcpu合成経路の削除) |
| C | 成功 | 下記 | 下記 | [C](#c書き出しの契約) |
| D | 成功（7/7行。2行目は部分検証） | 下記 | 下記 | [D](#d保存復旧の障害試験) |
| E | 実行中（修正1・2件目はマージ済み、3件目はPR待ち） | 下記 | 下記 | [E](#e性能は使われる経路から測る) |
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
| ジャーナル追記中 | 部分検証 | [不完全な末尾と復旧後の追記](#d-2ジャーナルの不完全な末尾)。実際の書き込み命令中の強制終了は未検証 |
| チェックポイント切替中 | 成功（欠陥1件を修正） | [下記](#d-3チェックポイント切替中) |
| `.altoproj` 保存のmanifest確定前後 | 成功 | [下記](#d-4altoproj-保存のmanifest確定前後) |
| OPFSのGC削除中 | 成功 | [下記](#d-5opfsのgc削除中) |
| 版のマージ確定中 | 成功 | [下記](#d-6版のマージ確定中) |
| Worker停止、GPU喪失 | 成功（欠陥4件を修正） | [下記](#d-7worker停止gpu喪失)。実機のGPU喪失と、例外を出さないWorkerの終了は未検証 |

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

### D-2：ジャーナルの不完全な末尾

`appendPartial` は、実際の追記対象からレコードを作り、ヘッダー途中・ペイロード途中まで、またはCRCを壊した全長のレコードを OPFS に書いて `flush` する。テスト専用の待機点で書き込みの成功通知を止め、CDPの `Page.crash` と `Target.closeTarget` の両方でタブを終了する。直前のdurableな状態について snapshot hash・state hash・タイル・全ブランチの履歴を照合する。その後に編集して `durable()` を待ち、もう一度タブを終了して再復旧し、切り詰め後の追記も照合する。

これは不完全な末尾を明示的に書き出してから終了する試験であり、OSやブラウザの書き込み命令の最中の中断、電源断、書き込み順序の保証を再現したものではない。実際の追記中に強制終了する試験は未検証として残す。

2026-11-10、`d41a0d4` を基準とする本変更、macOS 27.2 / Apple M5、Chrome 157.0.8103.39：

| コマンド | 結果 |
|---|---|
| `sh scripts/check.sh wasm-fast` | 成功。SDKビルド・API検査・単体テスト54件成功。WASMハッシュは既存の release 基準と不一致（fastでは報告のみ） |
| `ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`（Metal） | 120件成功。既存3条件と新規3条件、それぞれ2終了方法×10回 |
| `ALTO_BROWSER_GPU=swiftshader ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash` | 120件成功。同じ3種類の不完全な末尾と復旧後の追記を含む |

### D-3：チェックポイント切替中

チェックポイントは、まだ記録にない物体を新しいpackに書いてflushし、その後に使っていない側のジャーナルファイル（`journal-a.bin` / `journal-b.bin`）を新しい世代で書き直す（`storage-core.ts` の `checkpoint` と `writeGeneration`）。この切り替えの3時点でタブを終了した。

| 時点 | 止める場所 | 期待 |
|---|---|---|
| `checkpoint-pack` | packをflushした後、ジャーナルに書く前 | 前の世代と、その後に追記した記録 |
| `checkpoint-journal` | 新しい世代のヘッダーと記録の前半をflushした後（壊れた世代） | 同上。壊れた世代は候補にしない |
| `checkpoint-flushed` | 新しい世代を最後までflushした後。完了の通知は受け取らない | 新しい世代 |

作る状態：durableな基準の状態から書き込みを止め、層名の変更（追記として止まる）、ストローク、版の確定（`versions.commit`）を行う。確定によるチェックポイントは、待っていた2件の追記（ストロークと確定）を置き換える。書き込みを再開すると層名の変更が追記され、その後チェックポイントが指定の時点で止まる。前2時点は「基準＋層名の変更」、`flushed` は「確定後」の状態になれば合格で、反対側の状態だった場合は失敗として報告する。復旧後には層名の変更と版の確定をdurableにし、もう一度タブを終了して、古いpackや壊れた世代が残っていても次の世代が復旧することを確かめた。

待機点：`testing` 入口に `stopCheckpoint(stage)` と `checkpointStopped()` を足した。`partialAppend` と同じく、止めた後の書き込みは再開しない。本番の公開APIは変えていない。

#### 見つかった欠陥：確定後に編集がないと、復旧が提示されない

最初の実行で、3時点×2方法の6件が `no recovery journal is waiting` で失敗した。前2時点は1回目の復旧（前の世代）には成功し、2回目（復旧後の確定の後）で失敗した。

原因：`inspect` が `available: !!active?.recordCount` としていて、最新の世代がチェックポイントだけで、その後の記録が0件だと `recovery.available` が `false` になる。チェックポイントは版の確定、版の整理、物体の取り込みでも書かれる。そのため、確定した後に編集しないままタブを閉じると、次のページは復旧を提示しない。[埋め込みの手順](../embedding.md#復旧)どおりなら `createDocument` が呼ばれ、ファイルに保存していない作業が失われる。強制終了だけでなく、`Target.closeTarget`（利用者がタブを閉じる操作）でも起きる。

修正：

- チェックポイントのメタデータ（形式3・4）に `dirty` を記録する。値は、作成・ファイルを開く・保存の後から文書が変わっているかどうか（WASMの `isDirty`。`dirty` と同じ比較）。
- `available` は「最後のチェックポイントの後に記録がある」または「最新のチェックポイントが `dirty`」のとき `true`。保存直後・ファイルを開いた直後・空の新規文書は、これまでどおり `false`。`dirty` を持たない古いチェックポイントは、記録の有無だけで判定する（従来どおり）。
- 復旧で文書を読み込むと、読み込んだ状態が「保存済み」の基準になり `dirty` が消えていた。`dirty` なチェックポイントから復旧したときは `markUnsaved` で未保存に戻す。これがないと、復旧後に確定したチェックポイントが `dirty: false` で書かれ、次の再読み込みで同じ欠陥が起きる。
- 試験の比較に `dirty` を加えた（保存していない作業を、復旧後に保存済みとして示さないこと）。

修正の2か所はそれぞれ必要なことを確かめた。`available` の判定を戻すと6件が `no recovery journal is waiting` で失敗する。`markUnsaved` を外すと6件が `dirty` の不一致で失敗する（`flushed` は1回目、前2時点は2回目の復旧）。

`dirty` は記録の形式に追加したフィールドで、古い読み手は無視する。未リリースのため移行は不要（`RELEASING.md`）。

#### 検証

2026-11-10、`a32d9c1` を基準とする本変更。macOS 27.2 / Apple M5、Chrome 157.0.8103.39。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `sh scripts/check.sh native` | 0 | fmt・clippy成功。Rustテスト337件成功、失敗0 |
| `sh scripts/check.sh wasm-fast` | 0 | WASM clippy、SDKのビルド・API検査成功。単体テスト54件成功。releaseハッシュは基準と不一致（fastでは報告のみ。基準値は更新していない） |
| `ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`（Metal） | 0 | 180件成功（9時点×2方法×10回）。チェックポイントの3時点は1件376〜881 ms |
| 同上、`ALTO_BROWSER_GPU=swiftshader` | 0 | 180件成功。チェックポイントの3時点は1件4.9〜5.5 s |
| `node scripts/verify-browser.mjs --functional`（Metal） | 0 | 9群・305件成功（crash 18件を含む）。GPUプロセスのクラッシュ0件 |
| 同上、`ALTO_BROWSER_GPU=swiftshader` | 0 | 9群・305件成功。GPUプロセスのクラッシュ0件 |

各待機点で、2つの終わらせ方それぞれ10回連続で成功した。ブラウザ試験のWASMは `eb6fa4d9…`。CIは反復1回で、crashは18件になる。

これは書き込みの間で止めて終了する試験で、OSやブラウザの書き込み命令の最中の中断、電源断、flushの順序保証は再現していない。生ログはローカルの `verification/out/d3-a32d9c1-wip/`。

opencode（deepseek-v4.1-flash-free）のレビューは4点を挙げた。直したのは2点。`checkpointStopped()` は、止める設定を書き込みの開始時に消していたため、書き込みの途中で呼ぶと待たずに `CONFLICT` になる。止めた時点で消すようにした（試験専用の入口）。`flushed` で前の世代が復旧したときの失敗メッセージが「durableにならなかった編集」と誤って示すため、`flushed` では示さないようにした。修正後、`ALTO_CRASH_REPEAT=10` のMetalで180件、SwiftShaderの反復1回で18件、`check.sh wasm-fast` が成功した。ほかの2点は修正不要と判断した。`getSnapshot().dirty` は実行時の `dirty` そのもので、`markUnsaved` を外すと検出できることを上で確かめている。保存の後に編集があると古いチェックポイントの `dirty: true` が残るという指摘は、その場合は文書がもともと未保存なので誤りではない。

### D-4：`.altoproj` 保存のmanifest確定前後

`saveFolder` は、内容で名前が決まる物体（`objects/`・`snapshots/`・`commits/`）を書き、`manifest.json` を置き換え、読み戻して確かめた後に比較用の `document.json` を書く（`document.ts` の `saveFolderLocked`）。manifestの置き換えが確定点で、OPFSでは `createWritable()` の `close()` で原子的に置き換わる。v1を保存した後、v2（ストローク、層名の変更、版の確定）の保存を3時点で止めてタブを終了した。

| 時点 | 止める場所 | 期待 |
|---|---|---|
| `folder-before` | 物体を書いた後、manifestを書く前 | v1 |
| `folder-mid` | プロバイダー自身のmanifestの書き込みで、`close()`（置き換え）が返らない | v1 |
| `folder-after` | manifestを置き換えた後（読み戻し・ミラー・保存済みの通知の前） | v2 |

- 保存先は `OpfsFolderProvider` を試験側で包んだもの。試験用のフックはSDKに足していない。止めた保存はフォルダーのWeb Lockを持ったままになり、タブの終了でブラウザが解放する。
- 判定：新しいタブで、記録を持たないエンジンからフォルダーを開く。snapshot hash、`stateHash`、層のタイルマップ、版履歴（DAG、全ブランチ）、`dirty`（開いた文書は保存済み）を比べる。v1・v2のどちらかと完全に一致する場合だけ合格にし、反対側の版だった場合は失敗として報告する。
- 復旧後に版を確定し、死んだタブが残したフォルダー（書きかけの物体、閉じていない書き込み）へ同じプロバイダーで保存する。その後もう一度タブを終了し、開き直して照合する。2回目の保存は止めずに完了させるため、置き換えの途中で2回中断する場合は試していない。

試験が失敗を検出できることも確かめた。`folder-mid` で `close()` を通してから止めると、2件が「反対側の版を持っている」として失敗した。

欠陥は見つからなかった。

検証：

2026-11-10、`f46062b`（PR #95）を基準とする本変更。macOS 27.2 / Apple M5、Chrome 157.0.8103.39。SDKとRustのコードは変えていないので、`check.sh native` と `wasm-fast` は再実行していない。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`（Metal） | 0 | 240件成功（12時点×2方法×10回）。フォルダーの3時点は1件452〜855 ms |
| 同上、`ALTO_BROWSER_GPU=swiftshader` | 0 | 240件成功。フォルダーの3時点は1件4.8〜5.3 s |
| `ALTO_BROWSER_GPU=swiftshader node scripts/verify-browser.mjs --functional` | 0 | 9群・311件成功（crash 24件を含む）。GPUプロセスのクラッシュ0件 |

各待機点で、2つの終わらせ方それぞれ10回連続で成功した。CIは反復1回で、crashは24件になる。

対象はOPFSのフォルダーだけ。利用者が選んだフォルダー（`FileSystemDirectoryProvider`）は、ほかのプログラムが書き換えられるので対象外。OSの書き込みの途中での中断や電源断は再現していない。生ログはローカルの `verification/out/d4-f46062b-wip/`。

opencode（deepseek-v4.1-flash-free）のレビューは5点と軽微な2点を挙げた。直したのは3点。`folder-mid` が試験側で自分の書き込みを行い、プロバイダーの書き込み経路を通っていなかった。プロバイダーの `createWritable()` を包み、`close()` だけを止めるようにした。保存が止まる前に失敗または完了すると、試験が止まったままになっていた。`Promise.race` で失敗として返すようにした。開き直すときにフォルダーを作り直していたので、作らないようにした。修正後に上の検証を行った。ほかは誤りか修正不要と判断した。view はメソッドを持つただのオブジェクトなので、展開しても欠けない（v1の保存は成功している）。保存はプロバイダーの `exclusive` を通るので、包んだ経路を外れない。2回目の保存を止めないことは、上に制約として書いた。

### D-5：OPFSのGC削除中

`versions.gc({ dryRun: false })` は、フォルダーの排他ロックの中で次の順に進む（`document.ts` の `gcFolder`）。`.alto-gc.json` に `prepared` を書き、新しいmanifestが指す物体を書き、`manifest.json` を置き換える。置き換えが確定点で、文書は新しいmanifestを採用する（`adopt_gc`）。その後、不要な物体を1件ずつ削除し、全体を読み直して確かめ、比較用ファイルを書き、`.alto-gc.json` に `complete` を書く。

ブランチを削除して到達できなくなった版を2つ持つフォルダーを保存し、プレビューで2件が対象になることを確かめてから、GCを4時点で止めてタブを終了した。

| 時点 | 止める場所 | 期待 |
|---|---|---|
| `gc-before` | 物体を書いた後、manifestを書く前 | GC前と同じ |
| `gc-mid` | プロバイダー自身のmanifestの書き込みで、`close()`（置き換え）が返らない | GC前と同じ |
| `gc-delete` | 1件目の削除の後、2件目の前 | 採用後の状態 |
| `gc-complete` | 削除と確認の後、`complete` を書く前 | 採用後の状態 |

- 保存先はD-4と同じく、`OpfsFolderProvider` の `exclusive` が渡すviewを試験側で包んだもの。SDKに試験用のフックは足していない。
- GCの採用は、版の参照の世代（`generation` と `gc_generation`）を1つ進める（`crates/alto-engine/src/versions/gc.rs` の `adopt_gc`）。`stateHash` は版の参照を含むので、確定点の後では変わる。これは仕様で、欠陥ではない。確定後の2時点では、止めたタブが採用後に持っていた `stateHash` を期待値にし、ほかの項目はGC前と同じことを求める。
- 判定：新しいタブで、記録を持たないエンジンからフォルダーを開く。snapshot hash、`stateHash`、層のタイルマップ、版履歴（DAG、全ブランチ）、`dirty` を比べる。さらに、到達できるすべての版を1つずつ復元し、snapshot hashとタイルマップがGC前と同じことを確かめる（各root）。開いたフォルダーの文書は物体を必要になったときに読むので、復元はフォルダーに残った物体の中身を読む。復元は毎回Undoで戻し、戻った状態が全項目で同じことも確かめる。
- 復旧後にもう一度GCをかける。報告が実行済みで、文書が保存済みになり、新しいmanifestを採用し（`stateHash` が動く）、`.alto-gc.json` が `complete` になり、不要な2つの版のファイルが残らないことを確かめる。その後もう一度タブを終了し、開き直して全項目と各rootを照合する。

試験が欠陥を検出できることも確かめた。SDKのGCが使用中の物体を1つ余分に削除するように一時的に変えると、8件すべてが `history object missing` で失敗した。変更は戻した。

欠陥は見つからなかった。

検証：

2026-11-12、`cdb9b53`（main、PR #96のマージ後）を基準とする本変更。macOS 27.2 / Apple M5、Chrome 157.0.8103.39。SDKとRustのコードは変えていないので、`check.sh native` と `wasm-fast` は再実行していない。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`（Metal） | 0 | 320件成功（16時点×2方法×10回）。GCの4時点は1件565〜1141 ms |
| 同上、`ALTO_BROWSER_GPU=swiftshader` | 0 | 320件成功。GCの4時点は1件5.0〜6.7 s |
| `ALTO_BROWSER_GPU=swiftshader node scripts/verify-browser.mjs --functional` | 0 | 9群・319件成功（crash 32件を含む）。GPUプロセスのクラッシュ0件 |

各待機点で、2つの終わらせ方それぞれ10回連続で成功した。CIは反復1回で、crashは32件になる。

対象はOPFSのフォルダーだけ。2回目のGCは止めずに完了させるため、GCが2回続けて中断する場合は試していない。止めたGCが残した `.alto-gc.json`（`prepared`）は開き直したときに読まれず、次のGCが上書きする。生ログはローカルの `verification/out/d5-cdb9b53-wip/`。

opencode（deepseek-v4.1-flash-free）のレビューは4点を挙げた。直したのは3点。2回目のGCは不要な版のファイルが消えたことしか見ておらず、`gc-complete` では何もしないGCでも合格していた。報告、保存済み、採用、`complete` の記録も確かめるようにした。各rootの確認の後、snapshot hashだけで元に戻ったと判断していた。全項目で比べるようにした。版の並び順に依存しないよう、hashで並べるようにした。修正後に上の検証を行った。「`exclusive` を通らずにプロバイダーを包んでいる」という指摘は誤り。包むのは `exclusive` が渡すviewで、GCはロックの中で動く。

### D-6：版のマージ確定中

`versions.prepareMerge(name)` は3-wayマージの計画（競合の一覧とtoken）を作るだけで、文書も履歴も変えず、記録も書かない。`versions.finishMerge(token, resolutions)` が競合の解決を適用して二親の版を作り、Workerはそれをジャーナルの記録1件として書く（`worker.ts` の `historyCommand`）。版の確定（`commit`）と違い、チェックポイントは書かない。

"side" と main の両方が同じ層に描き、"side" は層名も変えた文書で、"side" を main へマージした。競合はすべて "theirs" で解決する。マージを次の4時点で止めてタブを終了した。

| 時点 | 止める場所 | 期待 |
|---|---|---|
| `merge-prepared` | 計画を作った後、確定の前 | マージ前 |
| `merge-applied` | 確定した後、記録の書き込みをメモリに保留したまま | マージ前 |
| `merge-torn` | 確定した後、記録の途中まで書いて止める | マージ前 |
| `merge-durable` | 確定して `durable()` が解決した後 | マージ後 |

- 判定：新しいタブでジャーナルから復旧する。snapshot hash、`stateHash`、層のタイルマップ、版履歴（DAG、全ブランチ）、`dirty` に加え、版の状態（ブランチの参照、head、未確定の変更）と、到達できるすべての版を1つずつ復元した内容（各root）を比べる。どちらか一方と完全に一致する場合だけ合格にし、反対側だった場合は失敗として報告する。
- `merge-durable` では、マージの前後でチェックポイントの世代が変わらないことも確かめる。復旧はマージの記録を再生する。
- 2回目：マージを失った時点ではもう一度マージし、二親の版になることを確かめる。マージが残った時点では、その上に描いて版を確定する（このときチェックポイントが書かれる）。その後もう一度タブを終了し、復旧して全項目を照合する。
- 試験が欠陥を検出できることも確かめた。SDKがマージの記録を捨てるように一時的に変えると、`merge-durable` の2件が失敗した。変更は戻した。

欠陥は見つからなかった。途中で、各rootの確認で行う復元とUndoの記録が `durable()` を待たずに残り、復元だけが書かれた状態で終了して不一致になった。これは試験側の問題で、確認の後に `durable()` を待つようにした（書き込みを保留している時点では待たない）。復元の記録だけが復旧されたのは、書けた分までを戻すという仕様どおりの動作。

調査用に、`ALTO_CRASH_ONLY=<正規表現>` で一部の待機点だけを実行できるようにした。

検証：

2026-11-12、`b9f0363`（main、PR #97のマージ後）を基準とする本変更。macOS 27.2 / Apple M5、Chrome 157.0.8103.39。SDKとRustのコードは変えていないので、`check.sh native` と `wasm-fast` は再実行していない。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`（Metal） | 0 | 400件成功（20時点×2方法×10回）。マージの4時点は1件511〜954 ms |
| 同上、`ALTO_BROWSER_GPU=swiftshader` | 0 | 400件成功。マージの4時点は1件5.0〜6.1 s |
| `ALTO_BROWSER_GPU=swiftshader node scripts/verify-browser.mjs --functional` | 0 | 9群・327件成功（crash 40件を含む）。GPUプロセスのクラッシュ0件 |

各待機点で、2つの終わらせ方それぞれ10回連続で成功した。CIは反復1回で、crashは40件になる。

対象はジャーナルからの復旧だけ。マージした文書を `.altoproj` へ保存している途中の中断はD-4の範囲で、マージした版で個別には試していない。競合の解決は "theirs" だけで、"ours" と明示値は試していない。生ログはローカルの `verification/out/d6-b9f0363-wip/`。

opencode（deepseek-v4.1-flash-free）のレビューは2点を挙げたが、どちらも誤りと判断した。`versions.status()` は同期関数なので、`await` しなくても中身が比べられている（失敗時の出力に中身が出ていた）。`branches()` の最初の版の確定は、直前にストロークがあるので空にならない。

### D-7：Worker停止、GPU喪失

SDKには描画用のエンジンWorkerがあり、その中に保存用のStorage Workerと画像のデコード用Workerがある。契約は次のとおり。

- エンジンWorkerが止まった：ページが再起動し、確定済みのジャーナルから同じ文書ハンドルへ戻す。状態は `restarting` から `ready` になる。途中の要求は `WORKER_CRASHED` で拒否する。
- 再起動に失敗した：状態は `failed` になり、編集を `WORKER_UNAVAILABLE` で拒否する。`retry()` で再試行できる。
- Storage Workerが止まった：状態は `storage-failed`（`STORAGE`）になる。編集は拒否するが、表示中の文書は `exportEmergency()` で書き出せる。`recoverAfterStorageFailure()` で確定済みの内容へ戻す。
- デコード用Workerが止まった：同じスレッドでのデコードに切り替え、エンジンは止めない。
- GPUを失った：CPU側の文書からデバイスを作り直す。描画中のストロークは `GPU_LOST` で終わる。ジャーナルには影響しない。

確定済みの作業と、書き込みをメモリに保留したままの編集がある状態で、次の7時点の障害を起こした。各時点では、同じタブで契約どおりの状態通知と拒否を確かめ、確定済みの内容が残ったことを照合する。その後、durableな編集を1件してからタブを終了する。

| 時点 | 起こす障害 | 同じタブで確かめること |
|---|---|---|
| `worker-error` | エンジンWorkerの中で未捕捉の例外を投げる（CDPでWorkerに接続して実行） | `restarting`→`ready`、保留中の `durable()` は `WORKER_CRASHED`、同じハンドル、確定済みの状態 |
| `worker-panic` | Rustのpanic（試験用の `testing.panic()`） | 同上。panicした要求も `WORKER_CRASHED` |
| `worker-failed` | 再起動時にWorkerのスクリプトを読めない | `failed`、編集は `WORKER_UNAVAILABLE`。この状態のままタブを終了する |
| `storage-error` | Storage Workerの中で未捕捉の例外を投げる | `storage-failed` だけになり再起動しない、編集は `STORAGE`、非常用の書き出しが空でない、`recoverAfterStorageFailure()` で確定済みの状態 |
| `decode-error` | デコード用Workerの中で未捕捉の例外を投げる | 状態が変わらない、文書が変わらない |
| `gpu-stroke` | 描画中のストロークでGPUデバイスを破棄する | ストロークは `GPU_LOST`、文書はストローク前のまま、復元後に描ける |
| `gpu-held` | 書き込みを保留している間にGPUデバイスを破棄する | 文書は変わらず、保留を解くと編集がdurableになる |

- 判定：新しいタブでジャーナルから復旧し、D-1と同じ項目（snapshot hash、`stateHash`、層のタイルマップ、版履歴、`dirty`）を比べる。2回目は名前の変更をdurableにしてもう一度タブを終了し、照合する。
- GPUは、注入した喪失の回数と `gpuLost` 通知の回数が一致しない場合に失敗にする。復旧が成功しても、注入していない喪失は合格に数えない。

欠陥4件を見つけて直した。

1. **Rustのpanicの後、エンジンが応答しなくなる。** リリースビルドはpanicで中断し、JSには `RuntimeError: unreachable` として届く。Workerはこれを1件の要求の失敗（`INTERNAL`）として返し、そのまま動き続けた。Runtimeは借用されたまま、メモリも途中の状態で残るため、以降の要求には返事がなく、状態は `ready` のままだった。WASMのトラップ（`WebAssembly.RuntimeError`）を受けたらWorkerはメッセージの受け付けをやめ、例外を捕捉せずに投げ直すようにした。ページ側は既存の経路で再起動し、ジャーナルから戻す（`worker.ts` の `trap`）。復旧候補を順に試す処理でも、トラップは壊れた候補として扱わず、次の候補を試さない。
2. **Storage Workerが止まると、`storage-failed` を飛ばしてエンジン全体が再起動する。** 保存用のセッションはエラーを受けて `storage-failed` にするが、イベントを処理済みにしていなかった。そのためエラーはエンジンWorkerを経てページまで伝わり、ページはエンジンWorkerが止まったとして再起動していた。結果として、durableでない表示中の編集が非常用の書き出しの機会もなく捨てられ、状態は `ready` に戻っていた。`onerror` で `preventDefault()` を呼ぶようにした（`storage-session.ts`）。
3. **デコード用Workerが止まると、エンジン全体が再起動する。** 同じ原因で、同じスレッドでのデコードに切り替える設計が働く前に再起動していた。同じ修正をした（`worker.ts` の `startDecoders`）。
4. **GPUを失ったときのストロークが `GPU_LOST` ではなく `INTERNAL` で終わる。** ストロークを終える際の読み戻しでmapが失敗し、デバイス喪失の通知より先に、その失敗が `INTERNAL: readback map` として返っていた。WebGPUがmapを拒否するのは、デバイスを失ったか、バッファが先に破棄・unmapされたときだけなので、`GPU_LOST` として返すようにした（`alto-gpu` の `readback.rs`）。キャンセルはその前に `CONFLICT` として返る。文書はどちらでもストローク前のまま残っていた。

panicを起こす試験用の入口として、Rustの `Runtime.testPanic` と `testing(engine).panic()` を足した。本番の公開APIには足していない。`testing` の他の入口（`destroyGpu` など）と同じ扱い。WorkerへのCDP接続のため、`verify-browser.mjs` の `connect` の `send` に `sessionId` を渡せるようにした。

修正前の試験は、1〜4のそれぞれで失敗した（panic後の応答待ちのタイムアウト、`storage-failed` の待ちのタイムアウト、デコード用Workerの障害後の「restarting」、`INTERNAL`）。最初から通った時点が欠陥を検出できることも確かめた。ページがWorkerのエラーで再起動しないように変えると `worker-error` が、GPUを失ったときに保留中の書き込みを捨てるように変えると `gpu-held` が失敗した。変更は戻した。

試験側では、エンジンの起動時に作られるデコード用Workerが止まったときに、あとから作り直されるかは確かめていない。止まった後は同じスレッドでデコードし続ける設計で、文書が変わらないことだけを確かめた。

検証：

2026-11-13、`e46382c`（main、PR #100のマージ後）を基準とする本変更。macOS 27.2 / Apple M5、Chrome 157.0.8103.39。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `cargo fmt --all -- --check`、`cargo clippy -p alto-gpu -p alto-wasm --all-targets --locked -- -D warnings`、`cargo test -p alto-gpu --locked` | 0 | 138件成功 |
| `cargo clippy --workspace --target wasm32-unknown-unknown --locked -- -D warnings` | 0 | 警告なし |
| `node --test packages/alto-sdk/test/*.test.js` | 0 | 54件成功 |
| `ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`（Metal） | 0 | 540件成功（27時点×2方法×10回）。この行の7時点は1件320〜934 ms |
| 同上、`ALTO_BROWSER_GPU=swiftshader` | 0 | 540件成功。この行の7時点は1件4.8〜6.8 s |
| `node scripts/verify-browser.mjs --functional`（Metal） | 0 | 9群・341件成功（crash 54件を含む）。GPUプロセスのクラッシュ0件 |

各待機点で、2つの終わらせ方それぞれ10回連続で成功した。CIは反復1回で、crashは54件になる。

GPUの喪失は `GPUDevice.destroy()` で起こしたもので、ドライバのリセットや外部GPUの取り外しなど実機の喪失は未検証。確認した環境はApple M5のMetalとSwiftShaderだけで、他のGPU・ブラウザには広げない。Workerの停止は未捕捉の例外とpanicで起こしたもので、Workerが例外を出さずに終わる場合（ブラウザによる強制終了）は起こせていない。その場合、エンジンWorkerへの要求は返事を待ち続ける（応答の監視はない）。生ログはローカルの `verification/out/d7-e46382c-wip/`。

opencode（deepseek-v4.1-flash-free）のレビューは4点を挙げたが、いずれも誤りと判断した。

- `storage-error` の確認が、後半の `ready` の判定と矛盾する：その前に `recoverAfterStorageFailure()` で `ready` に戻しているので矛盾しない。
- `durable` の処理の `catch` にトラップの扱いがない：この `catch` が受けるのはJSの `checkDocument` だけで、WASMは呼ばない。
- 入れ子のWorkerへのCDP接続が間に合わない：`setAutoAttach` は動いているWorkerにも接続する。Storage Worker・デコード用Workerの時点は、両環境で各40回成功した。
- 他の理由のmap拒否まで `GPU_LOST` になる：読み戻しのバッファをunmapするのは、先に `CONFLICT` を返すキャンセルと、待っている側がいなくなった後の `Lease` の破棄だけ。

あわせて、CI高速化のセッションからの依頼で、埋め込み試験の `event()` に待ち時間の引数を足し、back/forward cacheの2件の待ちを15秒から45秒にした。負荷の高いSwiftShaderのCIで、復帰が15秒に間に合わず1回失敗したため。判定は変えていない。

## E：性能は使われる経路から測る

計画の各項目の状態：

| 項目 | 状態 | 記録 |
|---|---|---|
| 固定fixtureと計測の仕組み | 成功 | [E-1](#e-1計測の仕組み) |
| 1回目の計測（main） | 成功。ただし main でテキスト・ベクター混在のfit表示が失敗する（最初の3回は3回とも、測り直しの3回は0回）。欠陥として記録 | [E-2](#e-2変更前の計測) |
| 支配的な処理の修正1件目：v3ベクターのラスタライズ | 成功（[#102](https://github.com/NobukiAkiyama/alto-engine/pull/102) でマージ） | [E-3](#e-3v3ベクターのラスタライズ)、[E-4](#e-4修正前後の比較) |
| 支配的な処理の修正2件目：v3ベクターのタイルごとの処理 | 成功（[#105](https://github.com/NobukiAkiyama/alto-engine/pull/105) でマージ） | [E-7](#e-7v3ベクターのタイルごとの処理修正2件目) |
| 支配的な処理の修正3件目：タイルに届かないパスを飛ばす | PR待ち | [E-8](#e-8タイルに届かないパスを飛ばす修正3件目) |
| 4件目以降 | 未着手 | [後続](#e-5見つかった欠陥と後続) |

### E-1：計測の仕組み

- B1〜B8（`verify-browser.mjs --benchmark-only`）を基準としてそのまま使う。足りない項目は新しい群 `tests/e2e/perf-tests.js` で測る。`node scripts/verify-browser.mjs --suite=perf` で単独実行する。`--functional` には含めない。`ALTO_PERF_FIXTURES` でfixtureを選び、`ALTO_PERF_STRESS=1` でストレス文書を足し、`ALTO_PERF_REPS`（JSON）で反復数を変える。CDPの待ち時間は、この群だけ4時間にした。
- 時間は、SDKを呼んでから `unstable(engine).settle()` が解決するまで（GPUの処理が完了したフレーム）のms。全サンプルを `perf.json` に保存する。p95は1回の実行で20サンプル以上の行だけ出し、3回分をまとめた値を示す。それ未満の行は中央値・最小・最大だけ出す。
- 固定fixture：

| 名前 | 種類 | 内容 |
|---|---|---|
| `4k-1` | ラスター | 3840×2160、画像の層1枚 |
| `4k-20` | ラスター | B1〜B8と同じ文書。全面の半透明層20枚 |
| `sparse` | ラスター | 8000×8000に小さな点を24個。大部分にタイルがない |
| `fx` | ラスター | 3840×2160の6層。不透明度、合成モード、ぼかし（半径8）、色調整、不透明度0.8の分離グループ、レベル補正の調整層 |
| `text-vector` | ベクター | 3840×2160。125パスのベクター層8枚と、テキスト層24枚（AltoTestLatin・AltoTestJP、横書きと縦書き） |
| `stress-64mpx`（ストレス） | ラスター | 8000×8000の層1枚を全面に描く。全タイルが異なる |

- 測る行：コールドの初回表示（等倍とfit。GPUのキャッシュだけを捨てる場合と、CPUのキャッシュも捨てる場合）、ウォームのパン1フレーム、ブラシの入力から表示まで、編集確定から再描画まで（小さなストローク、ベクターは色の変更、不透明度の変更）、PNG出力、`.alto` の保存、`.alto` を開く、開いて初回表示、復旧（Workerの再起動、ジャーナルの再生、初回表示）。開く・復旧では snapshot hash を照合し、PNG出力では大きさを確かめる。
- メモリは4時点（構築後、編集後、書き出し後、最後）で、次の値を分けて記録する。WASMメモリ（`wasmBytes`）、GPUリソースの計上値（`gpuBytes`。エンジンが数えた確保量で、物理VRAMの使用量ではない）、デコードプールの最大値、メインスレッドのJS heap（`performance.memory`）。Playgroundは `crossOriginIsolated` が false なので、Workerを含むJS heap（`measureUserAgentSpecificMemory`）は取れない。
- 全体のPNG出力は、64,000,000画素を超えるもの、CPU側の処理が512 MiBを超えるものを、SDKが拒否する（`worker.ts` の `exportImage`）。8000×8000の文書は倍率0.5で書き出す。
- 1つのfixtureが失敗しても、次のfixtureを続けて測る。終わった行は行ごとにレポートへ書き出すので、失敗やタイムアウトがあっても残る。群は最後に失敗として終わる。存在しないfixture名を混ぜて走らせ、残りの計測とレポートが残り、終了コードが1になることを確かめた。

環境：macOS 27.2、Apple M5（Metal）、Chrome 157.0.8103.39（headless）、rustc 1.99.1、Node v22.23.2。ウィンドウ1280×657、DPR 1、キャンバス970×468。表示はすべて物理画素で指定する（等倍は文書の1画素を画面の1画素に）。WASMは `sh scripts/build-m0.sh` と `npm run build`（`packages/alto-sdk`）でビルドした。sha256：main `c54aa67f…`、修正後 `d034a1a1…`。

計測中は他の重い処理を走らせていない。修正後の1回目（`e2-perf-fix-r1`）は、途中から無関係な行まで約10倍遅くなり、最後はストレス文書で4時間の上限に達した。text-vector のPNG出力は1867 ms（他の回は172〜174 ms）だった。開始から3分後に画面がスリープしていたが、修正後の2・3回目も画面がオフのまま正常な値だったので、原因は特定できていない。この回は比較から除き、`caffeinate -dimsu` を付けて4回目を測った。修正後の比較には2〜4回目を使う。

生ログはローカルの `verification/out/` にある。
- B1〜B8：`e0-bench-1693c34-r{1,2,3}`（変更前）、`e2-bench-fix-r{1..6}`（修正後）
- perf：`e1-perf-1693c34-r{1,2,3}` と `e1-perf-stress-1693c34-r{1,2,3}`（変更前）、`e2-perf-fix-r{1..4}`（修正後）
- 機能試験：`e3-functional-fix-metal`、`e3-functional-fix-swiftshader`

### E-2：変更前の計測

2026-11-13、main の `1693c34`（PR #101 のマージ後）に計測の群を足したもので実行した。B1〜B8 は3回、perf は3回。

| B1〜B8 | 変更前（3回） | 修正後（6回） |
|---|---|---|
| C2 GPUのコールド表示 ms | 294 / 297 / 285 | 290 / 292 / 293 / 276 / 279 / 279 |
| C3 CPU+GPUのコールド表示 ms | 291 / 278 / 286 | 294 / 291 / 288 / 278 / 277 / 277 |
| B1 GPU処理完了フレーム p95 ms | 4.8 / 5.8 / 4.6 | 8.2 / 6.5 / 5.8 / 5.0 / 4.0 / 4.8 |
| B2 ズーム ms | 4.6 / 4.0 / 4.0 | 3.3 / 3.6 / 2.8 / 4.1 / 3.2 / 3.3 |
| B3 連続入力から表示 p95 ms | 17.0 / 18.8 / 19.2 | 16.7 / 16.2 / 19.9 / 21.7 / 16.5 / 19.5 |
| B4 全面のUndo ms | 3.4 / 3.4 / 3.3 | 3.1 / 3.3 / 3.2 / 3.2 / 3.1 / 3.0 |
| B5 保存 ms | 22.6 / 22.3 / 21.5 | 21.8 / 21.9 / 21.2 / 21.6 / 21.1 / 21.2 |
| B5 開く ms | 1064 / 1063 / 1068 | 1067 / 1078 / 1061 / 1049 / 1055 / 1056 |
| B6 GPU計上値の最大 MiB | 493 / 493 / 493 | 493（6回とも） |
| B6 WASMメモリ MiB | 184 / 184 / 184 | 185 / 184 / 184 / 184 / 200 / 184 |
| B7 Worker再起動から初回描画 ms | 1410 / 1395 / 1374 | 1394 / 1455 / 1381 / 1368 / 1365 / 1363 |
| B8 小さなストロークの確定から再描画（等倍）ms | 9.2 / 9.3 / 9.2 | 9.3 / 12.7 / 9.0 / 9.1 / 9.2 / 9.1 |
| B8 同（全体）ms | 11.2 / 11.2 / 11.1 | 11.3 / 11.8 / 11.3 / 10.9 / 10.9 / 11.0 |

B1〜B8 の文書はラスターだけで、修正したベクターの処理を通らない。修正後の1回目は B1 p95 が8.2 msと変更前の範囲を超えたが、`caffeinate` を付けた4〜6回目は5.0 / 4.0 / 4.8で、再現しなかった。

perf の結果は [E-4](#e-4修正前後の比較) の表の「修正前」の列。要点：

- ラスターの4 fixture は、等倍での操作中の行（パン、ブラシ、小さなストロークの確定）がどれも中央値30 ms以下だった。fitでの小さなストロークの確定は sparse 約0.13秒、fx 約0.26秒、不透明度の変更は4k-20 約0.6秒。1秒を超えるのは4k-20の `.alto` を開く（約1.05秒）、開いて初回表示（約1.6秒）、復旧（約1.3〜1.4秒）だけ。
- text-vector は、等倍のコールド表示に約7.3〜7.6秒、fit のコールド表示・ベクターの色の変更・不透明度の変更・開いて表示・復旧がそれぞれ約61〜63秒かかった。最初の3回はfit表示で失敗した（[E-5](#e-5見つかった欠陥と後続)の1）。text-vector とストレス文書の「修正前」は、群を直した後に同じビルドで測り直した3回（`e1-perf-stress-1693c34-r1〜r3`）。
- メモリ（編集後）：WASMは4k-1 約68 MiB、4k-20 約171 MiB、sparse 約90 MiB、fx 約480 MiB、ストレス 約380 MiB。GPU計上値はどれも201〜205 MiB（固定のプールが大部分）。メインスレッドのJS heapは1〜9 MiBで、fx のあとだけ70〜139 MiBになった。WASMメモリは前のfixtureで伸びた分が残るので、同じ順序で測った回どうしでだけ比べる（text-vector は修正前の測り直しでは最初、修正後はfxの後に測った）。

### E-3：v3ベクターのラスタライズ

ラスターのfixtureでは、1秒を超えるのは `.alto` を開く・復旧だけで、操作中の行は最大でも約0.6秒（4k-20 のfitでの不透明度の変更）だった。text-vector は桁が違い、main では等倍のコールド表示に約7.3〜7.6秒、fit表示に約61〜63秒かかった（失敗する回もある。[E-5](#e-5見つかった欠陥と後続)）。テキストだけの半分（`diag-text-only`）は約0.3秒、ベクターだけの半分（`diag-vector-only`）は等倍で約7.2秒だった。遅いのはベクターのほうになる。

エンジンWorkerのCPUプロファイル（CDP、ベクターだけの文書の等倍コールド表示1回、7.5秒）：

| 関数 | 自己時間 |
|---|---|
| `raster_q8::rasterize_with` | 68.8% |
| `tile_renderer::Context::vector_q14` | 6.6% |
| `__rdl_alloc_zeroed` | 3.8% |

v3の整数ラスタライザは、タイルごと・パスごとに、256×256のウィンドウ全体のセルを積分していた。ウィンドウに届かないパスでも同じだった。1枚のタイルに対して1000パスあれば、6,500万セル分の128ビット加算と乗算になる。

直したこと（`crates/alto-gpu/src/raster_q8.rs`）：

1. 行ごとに、線分が書いた最初と最後のセルを覚え、その間だけを積分する。左側は和も面積も0なのでバイトは0（初期値のまま）。右側は面積が0で和が一定なので、すべて同じバイトになり、1回計算して埋める。オーバーフローの判定も、行全体を積分した場合と同じ箇所で同じエラーになる。
2. 非ゼロ規則で、すべての点（曲線の制御点を含む。平坦化した線分はこれに含まれる）がウィンドウのある辺の外側にあるパスは、何も覆わない。閉じたパスがウィンドウの左で交差しても、各行で打ち消し合う。こうしたパスは平坦化だけ行い（曲線の範囲・補間の範囲のエラーは従来どおり返す）、セルを保存も積分もしない。

出力のバイト列とエラーは、従来の計算と一致させる。新しい試験 `raster_q8_touched_cells_are_exact` は、決まった乱数で作った300パス（開いた・閉じたサブパス、直線、2次・3次曲線）を7つのウィンドウ（内側、またがる、位置と大きさが乱数のもの、完全に左・右・上・下）で、従来の計算と比べる。何かを描く組み合わせが600件を超えることも確かめる。右側を埋める処理を外す変更と、外側の判定を `xl + 512` にずらす変更は、どちらもこの試験で失敗した。

修正後のプロファイル（同じ条件、2.2秒）：`vector_q14` の自己時間22.4%、`__multi3` 9.3%、`raster_q8` の線分の処理5.9%、`integrate` 3.9%。残りは、タイルごとのパスの座標変換、64 KBのカバレッジが全部0かの確認、タイルごとに作る線の輪郭、確保の処理で、[後続](#e-5見つかった欠陥と後続)とする。

### E-4：修正前後の比較

修正前は E-2 の計測、修正後は PR #102（`claude/perf-vector-coverage`）の本体のコード（境界の試験を足す前の版、WASM `d034a1a1…`）の計測2〜4回目。各セルは3回の中央値の最小〜最大で、p95 は3回分の全サンプルから計算した。

#### 4k-1

| 行 | n×3回 | 修正前 | 修正前 p95 | 修正後 | 修正後 p95 |
|---|---|---|---|---|---|
| cold GPU first display (zoom 1) | 10 | 6.4–6.5 | — | 6.1–6.4 | — |
| cold CPU+GPU first display (zoom 1) | 10 | 6.2–6.5 | — | 6.1–6.3 | — |
| cold GPU first display (fit) | 10 | 46.5–47.0 | — | 44.2–45.9 | — |
| cold CPU+GPU first display (fit) | 10 | 44.9–46.1 | — | 43.6–45.3 | — |
| warm pan frame (zoom 1) | 120 | 2.4–3.0 | 3.6 | 1.8–2.4 | 3.2 |
| warm brush input-to-present | 30 | 3.9–4.7 | 7.5 | 3.6–4.8 | 6.6 |
| commit small stroke to redrawn (zoom 1) | 20 | 5.7–6.2 | 8.5 | 5.3–5.5 | 8.3 |
| commit small stroke to redrawn (fit) | 20 | 6.4–6.7 | 8.1 | 6.4–6.6 | 8.0 |
| commit opacity edit to redrawn (fit) | 20 | 32.6–33.1 | 43.2 | 32.6–33.1 | 43.8 |
| PNG export (scale 1) | 5 | 171–173 | — | 168–172 | — |
| save .alto bytes | 5 | 1.9–2.1 | — | 1.9–2.0 | — |
| open .alto | 5 | 55.8–56.0 | — | 54.0–55.2 | — |
| open .alto + first display (fit) | 5 | 91.3–94.3 | — | 88.9–91.2 | — |
| recover (Worker restart + journal replay + first display) | 5 | 147–148 | — | 142–143 | — |

#### 4k-20

| 行 | n×3回 | 修正前 | 修正前 p95 | 修正後 | 修正後 p95 |
|---|---|---|---|---|---|
| cold GPU first display (zoom 1) | 10 | 89.1–115 | — | 87.5–88.7 | — |
| cold CPU+GPU first display (zoom 1) | 10 | 89.6–136 | — | 86.4–87.0 | — |
| cold GPU first display (fit) | 10 | 636–668 | — | 618–626 | — |
| cold CPU+GPU first display (fit) | 10 | 634–662 | — | 619–628 | — |
| warm pan frame (zoom 1) | 120 | 2.9–3.5 | 3.9 | 2.8–2.9 | 3.3 |
| warm brush input-to-present | 30 | 10.6–12.0 | 22.3 | 11.0–12.1 | 22.4 |
| commit small stroke to redrawn (zoom 1) | 20 | 10.9–15.4 | 21.9 | 11.2–13.5 | 22.9 |
| commit small stroke to redrawn (fit) | 20 | 12.5–13.0 | 15.6 | 12.8–13.1 | 15.3 |
| commit opacity edit to redrawn (fit) | 20 | 593–626 | 1201 | 582–589 | 597 |
| PNG export (scale 1) | 5 | 173–184 | — | 162–164 | — |
| save .alto bytes | 5 | 19.9–21.7 | — | 19.8–20.0 | — |
| open .alto | 5 | 1046–1083 | — | 1034–1035 | — |
| open .alto + first display (fit) | 5 | 1622–1668 | — | 1594–1598 | — |
| recover (Worker restart + journal replay + first display) | 5 | 1306–1431 | — | 1271–1276 | — |

#### sparse

| 行 | n×3回 | 修正前 | 修正前 p95 | 修正後 | 修正後 p95 |
|---|---|---|---|---|---|
| cold GPU first display (zoom 1) | 10 | 1.7–1.9 | — | 1.7–1.8 | — |
| cold CPU+GPU first display (zoom 1) | 10 | 1.7–1.9 | — | 1.6–1.7 | — |
| cold GPU first display (fit) | 10 | 163–183 | — | 161–164 | — |
| cold CPU+GPU first display (fit) | 10 | 161–179 | — | 159–161 | — |
| warm pan frame (zoom 1) | 120 | 2.5–2.9 | 3.6 | 2.4–2.6 | 3.2 |
| warm brush input-to-present | 30 | 2.8–5.6 | 6.7 | 1.9–2.5 | 10.4 |
| commit small stroke to redrawn (zoom 1) | 20 | 4.6–4.9 | 8.9 | 4.0–4.4 | 5.6 |
| commit small stroke to redrawn (fit) | 20 | 133–141 | 173 | 131–136 | 168 |
| commit opacity edit to redrawn (fit) | 20 | 197–233 | 266 | 190–194 | 210 |
| PNG export (scale 0.5) | 5 | 379–390 | — | 348–371 | — |
| save .alto bytes | 5 | 1.8–2.0 | — | 1.7–1.8 | — |
| open .alto | 5 | 30.9–31.3 | — | 28.2–28.2 | — |
| open .alto + first display (fit) | 5 | 187–196 | — | 185–187 | — |
| recover (Worker restart + journal replay + first display) | 5 | 255–289 | — | 250–252 | — |

#### fx

| 行 | n×3回 | 修正前 | 修正前 p95 | 修正後 | 修正後 p95 |
|---|---|---|---|---|---|
| cold GPU first display (zoom 1) | 10 | 42.9–46.0 | — | 41.6–42.1 | — |
| cold CPU+GPU first display (zoom 1) | 10 | 42.6–46.4 | — | 42.1–42.5 | — |
| cold GPU first display (fit) | 10 | 342–371 | — | 336–356 | — |
| cold CPU+GPU first display (fit) | 10 | 357–368 | — | 338–352 | — |
| warm pan frame (zoom 1) | 120 | 2.7–3.1 | 3.7 | 2.5–2.7 | 3.3 |
| warm brush input-to-present | 30 | 11.1–11.9 | 25.3 | 10.9–11.2 | 20.5 |
| commit small stroke to redrawn (zoom 1) | 20 | 22.9–25.7 | 36.0 | 22.9–26.7 | 35.3 |
| commit small stroke to redrawn (fit) | 20 | 254–259 | 280 | 252–254 | 280 |
| commit opacity edit to redrawn (fit) | 20 | 262–287 | 354 | 255–258 | 282 |
| PNG export (scale 1) | 5 | 867–899 | — | 852–918 | — |
| save .alto bytes | 5 | 65.8–67.4 | — | 64.8–65.3 | — |
| open .alto | 5 | 603–651 | — | 595–600 | — |
| open .alto + first display (fit) | 5 | 847–877 | — | 839–841 | — |
| recover (Worker restart + journal replay + first display) | 5 | 737–799 | — | 723–727 | — |

#### text-vector

| 行 | n×3回 | 修正前 | 修正前 p95 | 修正後 | 修正後 p95 |
|---|---|---|---|---|---|
| cold GPU first display (zoom 1) | 3 | 7349–7441 | — | 2114–2132 | — |
| cold CPU+GPU first display (zoom 1) | 3 | 7243–7419 | — | 2119–2127 | — |
| cold GPU first display (fit) | 3 | 60914–63050 | — | 17352–17475 | — |
| cold CPU+GPU first display (fit) | 3 | 60963–63076 | — | 17307–17462 | — |
| warm pan frame (zoom 1) | 120 | 2.8–3.5 | 4.0 | 2.8–3.4 | 4.1 |
| commit vector recolor to redrawn (zoom 1) | 5 | 8211–8528 | — | 2425–2439 | — |
| commit vector recolor to redrawn (fit) | 5 | 61195–63444 | — | 17457–17540 | — |
| commit opacity edit to redrawn (fit) | 5 | 61764–62361 | — | 17501–17584 | — |
| PNG export (scale 1) | 5 | 174–177 | — | 172–174 | — |
| save .alto bytes | 5 | 23.9–24.0 | — | 24.3–24.6 | — |
| open .alto | 3 | 52.8–53.4 | — | 52.7–52.7 | — |
| open .alto + first display (fit) | 3 | 61586–62132 | — | 17518–17596 | — |
| recover (Worker restart + journal replay + first display) | 3 | 61551–62265 | — | 17522–17702 | — |

#### stress-64mpx（ストレス）

| 行 | n×3回 | 修正前 | 修正前 p95 | 修正後 | 修正後 p95 |
|---|---|---|---|---|---|
| cold GPU first display (zoom 1) | 3 | 6.1–6.2 | — | 6.1–6.4 | — |
| cold CPU+GPU first display (zoom 1) | 3 | 6.1–6.1 | — | 6.2–6.2 | — |
| cold GPU first display (fit) | 3 | 377–380 | — | 380–382 | — |
| cold CPU+GPU first display (fit) | 3 | 376–378 | — | 375–379 | — |
| warm pan frame (zoom 1) | 30 | 2.2–2.5 | 3.5 | 1.6–2.3 | 3.1 |
| warm brush input-to-present | 10 | 4.3–5.0 | — | 4.3–4.7 | — |
| commit small stroke to redrawn (zoom 1) | 5 | 10.1–10.8 | — | 9.5–10.2 | — |
| commit small stroke to redrawn (fit) | 5 | 258–259 | — | 254–259 | — |
| commit opacity edit to redrawn (fit) | 5 | 337–339 | — | 336–342 | — |
| PNG export (scale 0.5) | 2 | 515–520 | — | 490–498 | — |
| save .alto bytes | 2 | 9.1–9.7 | — | 8.9–9.0 | — |
| open .alto | 2 | 433–438 | — | 430–433 | — |
| open .alto + first display (fit) | 2 | 728–731 | — | 724–737 | — |
| recover (Worker restart + journal replay + first display) | 2 | 738–744 | — | 733–736 | — |

- text-vector：等倍のコールド表示は 7243〜7441 ms から 2114〜2132 ms に、fit のコールド表示は 60914〜63076 ms から 17307〜17475 ms になった。fit での色の変更・不透明度の変更・開いて表示・復旧も約62秒から約17.5秒になった。どれも3回のばらつき（修正前で最大約2.1秒、修正後で最大約0.2秒）を超えている。ウォームのパン、PNG出力、保存は変わらない。開く（表示なし）は52.9〜53.4 msから54.4〜55.6 msで、1〜2 ms大きい。開くだけでは描画しないので変えた経路を通らず、他の fixture の開くは差がない（4k-20 で1044〜1056 msと1040〜1043 ms）。
- ラスターの fixture とストレス文書は、修正前後で中央値の範囲が重なるか、修正後が少し小さい。修正したコードを通らないので、差は環境の揺れとみなす。p95 が1 msを超えて上がったのは sparse のブラシ（6.7→10.4 ms）だけで、修正後の3回目の2サンプル（16.1、18.0 ms）によるもの。他の2回は変更前と同じ（6.7、6.9 ms）なので、再現する劣化ではない。ほかに p95 が上がった行は、どれも1 ms以内。
- メモリは、同じ順序で測った fixture で、修正前後の差がない。

### E-5：見つかった欠陥と後続

1. **main で text-vector の fit 表示が `INVALID_ARGUMENT: v3 vector coverage exceeds GPU budget` で失敗することがある。** 1回目の計測（`e1-perf-1693c34-r1〜r3`）では3回とも、fit のコールド表示の最初のサンプルで失敗した。ベクターだけの文書を単独で実行しても失敗した。同じビルドで後から測った `e1-perf-stress-1693c34-r1〜r3` では、3回とも失敗しなかった。main でも起きる回と起きない回がある。修正後は、計測4回と試行2回で一度も起きていない。修正はカバレッジのバイト列も、テクスチャを確保するかの判断も変えないので、修正で解消したとは扱わない。判定は `tile_renderer/paint.rs` の `vector_q14` で、パスごとに確保するカバレッジのテクスチャと出力先が、GPU側で解放されるまで計上値に残ることに関係すると考えているが、確かめていない。再現の条件の特定を後続とする。
2. **ベクターの描画は修正後もfitで約17秒かかる。** 残りは、パスの座標変換をタイルごとに行うこと、64 KBのカバレッジが全部0かの確認、線の輪郭をタイルごとに作り直すこと（`stroke_q8::rasterize_stroke`）、パスごとのテクスチャと出力先の確保。次の修正の候補とする。→ [E-7](#e-7v3ベクターのタイルごとの処理修正2件目) で約8秒、[E-8](#e-8タイルに届かないパスを飛ばす修正3件目) で約3.7秒になった。
3. **外部の `.alto` を開く処理は、全オブジェクトを復号して照合する（`alto-store` の `decode_entries`）。** 4k-20で約1.03秒、fx で約0.6秒かかる。B5 の open（約1.06秒）と同じ処理になる。
4. **復旧（Workerの再起動、ジャーナルの再生、初回表示）は4k-20で約1.27秒。** B7（約1.37秒）と同じ傾向。
5. **20層の不透明度の変更から再描画まで、fitで約0.59秒。** 変更前は p95 が1201 msになった回があった。

3〜5はラスター側で、計測では1秒前後にとどまる。修正の順番は、1と2のベクター側を先にする。

### E-6：検証

修正（PR #102、`claude/perf-vector-coverage`）について、2026-11-13、上と同じ環境で実行。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `sh scripts/check.sh native` | 0 | 338件成功（`raster_q8_touched_cells_are_exact` を含む） |
| `cargo test -p alto-gpu --locked raster_q8` | 0 | 13件成功（境界の試験 `raster_q8_paths_on_the_window_edges_are_exact` を含む） |
| `node scripts/verify-browser.mjs --functional`（Metal） | 0 | 9群・341件成功（parity 164件、crash 54件を含む） |
| 同上、`ALTO_BROWSER_GPU=swiftshader` | 0 | 9群・341件成功（約10分） |

parity は、ベクター・テキストのタイルの画素をRustの基準と照合する。修正後もすべて一致した。

opencode（deepseek-v4.1-flash-free）のレビューは1点を挙げた。パスの右端がウィンドウの左端にちょうど接すると外側と判定され、全0になるが、従来の計算では0にならない、というもの。誤りと判断した。ウィンドウより左の線分は捨てずに左端の列へ寄せ（`ux <= xl && vx <= xl`）、閉じたパスでは打ち消し合う。判定の `<=` はこの寄せと同じ境界になる。境界にちょうど接するパスは乱数の試験ではほとんど出ないので、各辺に外側・内側から接する長方形と曲線（閉じた・開いた）を比べる試験を足した。外側の判定を `xl + 300` にずらすと、この試験は失敗した。

計測の群（`perf-tests.js` と `verify-browser.mjs` の変更）について、opencode は6点を挙げた。5点を直した。
- `ALTO_PERF_REPS` をランナーの読み込み時に解析していて、値が壊れていると他の群も動かなかった。ページ側で解析するようにし、壊れた値でも m8 の群が成功することを確かめた。
- `ALTO_PERF_FIXTURES` の空白や空の名前で失敗していた。前後の空白を除き、空なら既定の組にする。
- テキストだけの診断用fixtureで、空の色の変更を測った行が出ていた。ベクターの層がない場合は出さない。
- レポートの書き出しが失敗すると群が止まった。fixtureの `try` の中で書き出し、失敗時の書き出しの失敗は記録だけする。
- 反復数を検証していなかった（`export: 0` で出力を測らずに成功していた）。1以上の整数でなければ拒否する。

直さなかった1点：「コールド表示の1系列目の後に `enablePrefetch` を呼ぶので、2系列目以降は先読みが有効なまま測られる」。`resetRenderCache` は呼ぶたびに先読みを止める（`worker.ts`）ので、コールドのサンプルはすべて先読みなしで測られている。

この記録自体のレビューは5点を挙げた。本文と表の食い違い4点（操作中の行の上限、ウィンドウの数、p95が上がった行、ラスター側の項目の番号）と、状態の表の書き方を直した。

### E-7：v3ベクターのタイルごとの処理（修正2件目）

E-4 の修正後も、text-vector の fit 表示に約17秒かかった（[E-5](#e-5見つかった欠陥と後続)の2）。fit 表示は、レベル0の全タイル（3840×2160で135枚）を描いてから縮小する。どのタイルでも1000本のパスを、塗りと線の2回ずつ処理していた。

エンジンWorkerのCPUプロファイル（fit のコールド表示1回）では、まずパスごとの GPU の確保と1回ずつの描画を疑ったが、まとめて描くようにしても約17.1秒で、ほとんど変わらなかった。残りは CPU 側だった。

| 段階 | fit のコールド表示（試行1回） | プロファイルで大きかった処理 |
|---|---|---|
| main（`959c0bf`） | 約17.6秒 | — |
| 1. まとめて描く | 17.1秒 | カバレッジ全体の走査（自己時間23%） |
| 2. 書いた範囲だけ走査する | 13.3秒 | 線の輪郭を作る処理（含む時間40%） |
| 3. 輪郭とパスの変換をタイル間で使い回す | 6.9秒 | — |
| 4. 使い回す輪郭を詰めて持つ（採用） | 8.0秒 | — |

直したこと（`crates/alto-gpu`）：

1. **まとめて描く。** パスごとに 256×256 のカバレッジのテクスチャと出力先を確保して `vector-q14.wgsl` を1回ずつ実行していたのをやめた。0でない範囲だけを切り出し、テキストと同じ `glyph-q14.wgsl` で、1回の実行で最大1024個を順に重ねる（`paint.rs` の `paint_crops`。テキストと共有）。`glyph-q14.wgsl` は画素ごとに、切り出しを描く順に、`vector-q14.wgsl` と同じ式（同じ丸め）で前の値に重ねる。カバレッジが0の画素はどちらの式でも変わらないので、Q14 の結果は1個ずつ描いた場合と同じになる。parity はこの段階の後も全件一致した。`vector-q14.wgsl` は、タイル以外の経路でまだ使っている。
2. **書いた範囲だけ走査する。** ラスタライザが、0でないバイトを含む矩形を返すようにした（`raster_q8::rasterize_crop`）。非ゼロ規則では、線分が書いた行と列から求める。行の右側の一定値が0でなければ右端まで含める。切り出しは、この矩形の中だけを調べる。タイルに届かないパスは、64 KB を調べずに飛ばせる。
3. **タイル間で使い回す。** パスの座標変換（`map_path`）と線の輪郭（`stroke_q8::outline`）は、パス・線の設定・座標変換・レベルだけで決まり、タイルによらない。ノードのパスの一覧（`Arc`。線の設定もこの一覧の中にあり、変えると別の一覧になる）・座標変換・レベルごとに、`Resources` に保持する。項目は自分の `Arc` を持つので、同じアドレスは同じ中身の一覧だけを指す。8フレーム使わなかった項目と、32個を超えた分の古い項目は捨てる。ベクターマスクは1本のパスの一覧をタイルごとに作るので、保持しない。
4. **輪郭を詰めて持つ。** 3だけでは、輪郭が `Vec<Command>`（1命令56バイト）で1本あたり約47 KBになり、Worker再起動後の WASM メモリが main の12.1 MiBから67.0 MiBに増えた（`e5-perf-fix-r1〜r3`）。命令ごとのタグ1バイトと点の列で持つようにした（`raster_q8::Packed`）。線の命令は56バイトから17バイトになり、`Vec` を伸ばしたときの余りの容量も持たない。全ノードで32 MiBを超える分は保持せずにタイルごとに作るが、text-vector はこの上限に届かない。WASM メモリは23.0 MiBになり、増え方は約5分の1になった。タイルごとに命令へ戻す処理で約1秒遅くなったが、メモリを抑えるほうを選んだ。

試験：
- `raster_q8_crop_equals_cropping_the_whole_coverage`：決まった乱数で作った300パスを、2つの規則と7つのウィンドウ（0幅を含む）で、`rasterize_crop` と `Packed` 経由の結果が、全体を計算して切り出した結果と一致することを確かめる。何かを描く組み合わせが900件を超えることも確かめる。右側の一定値の判定を1列狭くする変更は、この試験で失敗した。
- `raster_q8_crop_keeps_every_nonzero_byte`：切り出しを元の位置に戻すと元のカバレッジになり、矩形の4辺がどれも0でないバイトを含むことを確かめる。

#### 修正前後の比較

修正前は main の `959c0bf`（WASM `b05a8a00…`）、修正後は `1e92fb3`（WASM `a8dcda5e…`）。修正前の `e5-perf-main-r1〜r3` は、上の段階3（詰める前）の修正と交互に3回ずつ測った。採用した段階4は、その直後に同じ機械で3回測った（`e7-perf-fix-r1〜r3`）。交互ではないので、main との差のうち環境の揺れの分は、ラスター側の行で確かめる（下の3つ目）。どちらもストレス文書を含む。各セルは3回の中央値の最小〜最大で、範囲は GPU と CPU+GPU の2行を合わせたもの。text-vector の表だけを載せる。他の fixture の表は、生ログの `perf.json` から同じ手順で作れる。

| 行 | n×3回 | 修正前 | 修正前 p95 | 修正後 | 修正後 p95 |
|---|---|---|---|---|---|
| cold GPU first display (zoom 1) | 3 | 2148–2220 | — | 991–1034 | — |
| cold CPU+GPU first display (zoom 1) | 3 | 2154–2227 | — | 1009–1040 | — |
| cold GPU first display (fit) | 3 | 17635–17927 | — | 8082–8170 | — |
| cold CPU+GPU first display (fit) | 3 | 17595–17870 | — | 8041–8147 | — |
| warm pan frame (zoom 1) | 120 | 3.1–3.7 | 4.6 | 3.0–3.3 | 4.1 |
| commit vector recolor to redrawn (zoom 1) | 5 | 2452–2525 | — | 1196–1232 | — |
| commit vector recolor to redrawn (fit) | 5 | 17781–17983 | — | 8199–8220 | — |
| commit opacity edit to redrawn (fit) | 5 | 17811–18060 | — | 8172–8215 | — |
| PNG export (scale 1) | 5 | 177–184 | — | 179–185 | — |
| save .alto bytes | 5 | 24.9–26.5 | — | 25.3–26.0 | — |
| open .alto | 3 | 52.9–53.4 | — | 54.4–55.6 | — |
| open .alto + first display (fit) | 3 | 17836–18142 | — | 8153–8227 | — |
| recover (Worker restart + journal replay + first display) | 3 | 17885–18157 | — | 8258–8322 | — |

- text-vector：fit のコールド表示は 17595〜17927 ms から 8041〜8170 ms に、等倍のコールド表示は 2148〜2227 ms から 991〜1040 ms になった。fit での色の変更・不透明度の変更・開いて表示・復旧も約18秒から約8.2秒になった。ウォームのパン、PNG出力、保存は変わらない。開く（表示なし）は52.9〜53.4 msから54.4〜55.6 msで、1〜2 ms大きい。開くだけでは描画しないので変えた経路を通らず、他の fixture の開くは差がない（4k-20 で1044〜1056 msと1040〜1043 ms）。
- メモリ：Worker再起動後（`final`）の WASM は、main 12.1 MiB、修正後 23.0 MiB（3回とも）。増えた約11 MiBは、使い回す輪郭とパスの分で、速さと引き換えにした（上の4）。GPU計上値は203.5 MiBで変わらない。他の時点の WASM は、前の fixture で伸びた分が残るので両方163.7 MiBになり、比べられない。
- ラスターの4 fixture とストレス文書は、どの行も修正前後で中央値の範囲が重なるか、差が2 ms未満だった（表は省略）。変えたのは v3 ベクターの経路だけなので、差は環境の揺れとみなす。
- GPU 予算超過のエラー（[E-5](#e-5見つかった欠陥と後続)の1）は、main 3回・段階3の3回・修正後3回の perf のどれでも起きなかった（B1〜B8 はベクターを描かない）。修正後はテクスチャと出力先の確保が、パスごとではなくまとめて描く単位ごとになるので起きにくくなるはずだが、main でも起きなかったので、解消したかは判断できない。

B1〜B8（`e5-bench-main-r1〜r3`、`e7-bench-fix-r1〜r3`）。ラスターだけの文書なので、修正した経路は通らない。

| B1〜B8 | main（3回） | 修正後（3回） |
|---|---|---|
| C2 GPUのコールド表示 ms | 291 / 288 / 284 | 287 / 297 / 283 |
| C3 CPU+GPUのコールド表示 ms | 294 / 293 / 289 | 288 / 297 / 285 |
| B1 GPU処理完了フレーム p95 ms | 6.0 / 3.2 / 4.7 | 5.4 / 5.9 / 6.0 |
| B2 ズーム ms | 3.2 / 2.3 / 2.1 | 2.5 / 4.1 / 4.1 |
| B3 連続入力から表示 p95 ms | 18.5 / 13.9 / 18.5 | 17.0 / 18.7 / 19.2 |
| B4 全面のUndo ms | 3.7 / 3.3 / 3.1 | 3.4 / 3.1 / 3.1 |
| B5 保存 ms | 21.6 / 22.2 / 21.5 | 22.0 / 21.0 / 21.2 |
| B5 開く ms | 1065 / 1075 / 1054 | 1059 / 1053 / 1053 |
| B6 GPU計上値の最大 MiB | 493（3回とも） | 493（3回とも） |
| B7 Worker再起動から初回描画 ms | 1415 / 1428 / 1373 | 1386 / 1367 / 1429 |
| B8 小さなストロークの確定から再描画（等倍）ms | 15.1 / 11.5 / 8.8 | 9.2 / 8.9 / 9.6 |
| B8 同（全体）ms | 18.3 / 15.1 / 10.8 | 11.3 / 10.9 / 12.0 |

B2 のズームは修正後のほうが少し大きい回があるが、E-2 の main の3回（4.6 / 4.0 / 4.0 ms）の範囲に収まる。B8 の main の1・2回目が大きいのは、その回だけの揺れで、3回目は修正後と同じだった。

生ログ：`e4-parity-v2〜v4`・`e4-perf-v2〜v4-try`・`e6-parity`・`e6-perf-try`（各段階の試行）、`e5-perf-{main,fix}-r1〜r3`・`e5-bench-{main,fix}-r1〜r3`（main と3の段階の交互の計測）、`e7-perf-fix-r1〜r3`・`e7-bench-fix-r1〜r3`（採用した修正）、`e8-functional-{metal,swiftshader}`・`e8-check-native.log`（検証）。

#### 検証

2026-11-14、上と同じ環境で、`1e92fb3` を実行。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `sh scripts/check.sh native` | 0 | 341件成功（新しい試験2件を含む） |
| `cargo test -p alto-gpu` | 0 | 142件成功 |
| `node scripts/verify-browser.mjs --functional`（Metal） | 0 | 9群・341件成功（parity 164件、crash 54件を含む） |
| 同上、`ALTO_BROWSER_GPU=swiftshader` | 0 | 9群・341件成功 |

parity は、ベクター・テキストのタイルの画素をRustの基準と照合する。1〜4の各段階の後にも parity を実行し、すべて一致した（`e4-parity-v2〜v4`、`e6-parity`）。

opencode（deepseek-v4.1-flash-free）のレビューは、差分が大きいと応答がなかったので、描画、ラスタライザとキャッシュ、詰めた形式の3つに分けた。描画は「なし」。残りの2つで挙がった5点は、どれも誤りと判断した。
- 「すべて0のときに `None` を返さない」：返す矩形は0でないバイトを含む上限で、すべて0なら切り出しが `None` を返す。何も書いていない行は矩形に入れないので、幅が負の矩形にはならない。ただ、関数の説明が「すべて0なら `None`」と読めたので、書き直した。
- 「`PREPARED_VECTORS` の上限が効いていない」「使ったときに `used` を更新していない」：どちらも `paint.rs` にあり、その部分の差分を渡していなかった。
- 「`Packed::new` がコンパイルできない」「借用の検査で失敗する」：ビルド・clippy・試験が通っている。
- 「容量の計算で自分の項目を二重に数える」：合計を出す前に、自分の項目を `vectors` から取り出している。

### E-8：タイルに届かないパスを飛ばす（修正3件目）

E-7 の後も、text-vector の fit 表示に約8秒かかった。縮小のレベルを直接描けば数枚のタイルで済むが、v3 のレベル1以上は子の整数ダウンサンプルとして定義している（[v3 整数合成](../design/2026-10-28-v3-integer-compositing.md)の DQ3-1。冷えた要求と温まった要求のハッシュを一致させるため）。この定義は変えず、レベル0のタイルを描く処理を減らした。

エンジンWorkerのCPUプロファイル（fit のコールド表示1回、8.0秒）では、線の輪郭のラスタライズ（`rasterize_crop_packed`）が含む時間の58%、そのうち線分の処理（`NonZero::segment`、`lerp_line`）が大部分だった。ほとんどのタイルには、ほとんどの輪郭が届かない。それでも、範囲の誤りを従来どおり返すために、届かないタイルでも全線分をたどっていた（E-3 の2）。

直したこと（`crates/alto-gpu/src/raster_q8.rs`）：

- 塗りのパスと線の輪郭を、ノードごとに一度だけ正規化して持つ（`raster_q8::Prepared`。E-7 の `Packed` を置き換える）。正規化した命令、原点、全点（制御点を含む）の外接矩形、「ウィンドウのある辺の外側にあるときの結果」を持つ。
- 外側にあるときの結果は、そうしたどのウィンドウでも同じになる。曲線は、始点の有無と分割数の検査（ウィンドウを読まない）の後、分割の前に、捨てるか弦に置き換えられる。全点が一つの辺の外側なら、どの曲線もそうなる。残る線分の補間（`lerp_line`、`lerp_column`、`div_q`）は、座標が 0〜2^60 なら `i128`・`i64` を超えない（補間した値は端点の間にある）。そこで、全点より右の1画素のウィンドウで一度だけたどって結果を覚え、外側のタイルではたどらずにその結果を返す。誤りも同じものを返す。座標がこの範囲を超えるパスは、従来どおりたどる。
- 結果を求めたときの分割数の上限も `Prepared` に持ち、すべてのウィンドウで同じ上限を使う。

試験：
- `raster_q8_prepared_outside_keeps_every_error`：始点のない曲線（`Range`）、上限を超える曲線（`CurveRange`）、線だけのパス、3次曲線、2^61 の座標、空のパスを、内側・上下左右の外側・遠いウィンドウ・幅0のウィンドウ、2つの規則で、従来の `rasterize_crop` と比べる。外側の近道を使う組み合わせが30件を超えることも確かめる。
- `raster_q8_crop_equals_cropping_the_whole_coverage` も `Prepared` を通す（E-7 の `Packed` の比較を置き換えた）。
- 外側の結果を常に成功とみなす変更は前者で、常に外側とみなす変更は後者で失敗した。

#### 修正前後の比較

修正前は E-7 の修正（`1e92fb3`、WASM `a8dcda5e…`）、修正後は `2e56bef`（WASM `fb180111…`）。両方を交互に3回ずつ測った（`e10-perf-{main,fix}-r1〜r3`、ストレス文書を含む。ここでの `main` は E-7 の修正の版を指す）。表の「修正前」は E-7 の修正。各セルは3回の中央値の最小〜最大で、範囲は GPU と CPU+GPU の2行を合わせたもの。text-vector の表だけを載せる。最後のコミット `9a0de0f`（レビューの2点、下の検証）は結果を変えないので3回は測り直していない。試行1回（`e11-perf-try`）では fit 3715〜3732 ms、等倍 462〜463 msで、等倍は表の範囲より10 msほど小さいが、1回の揺れの範囲とみなす。

| 行 | n×3回 | 修正前 | 修正前 p95 | 修正後 | 修正後 p95 |
|---|---|---|---|---|---|
| cold GPU first display (zoom 1) | 3 | 1016–1022 | — | 474–503 | — |
| cold CPU+GPU first display (zoom 1) | 3 | 1016–1033 | — | 490–498 | — |
| cold GPU first display (fit) | 3 | 8055–8084 | — | 3594–3756 | — |
| cold CPU+GPU first display (fit) | 3 | 8011–8096 | — | 3576–3798 | — |
| warm pan frame (zoom 1) | 120 | 3.2–3.8 | 4.8 | 3.2–3.6 | 4.3 |
| commit vector recolor to redrawn (zoom 1) | 5 | 1199–1213 | — | 603–613 | — |
| commit vector recolor to redrawn (fit) | 5 | 8179–8278 | — | 3649–3793 | — |
| commit opacity edit to redrawn (fit) | 5 | 8149–8192 | — | 3670–3785 | — |
| PNG export (scale 1) | 5 | 178–185 | — | 180–190 | — |
| save .alto bytes | 5 | 25.3–26.5 | — | 26.1–26.7 | — |
| open .alto | 3 | 54.9–56.7 | — | 57.2–59.3 | — |
| open .alto + first display (fit) | 3 | 7858–8176 | — | 3664–3797 | — |
| recover (Worker restart + journal replay + first display) | 3 | 8242–8286 | — | 3795–3927 | — |

- text-vector：fit のコールド表示は 8011〜8096 ms から 3576〜3798 ms に、等倍のコールド表示は 1016〜1033 ms から 474〜503 ms になった。fit での色の変更・不透明度の変更・開いて表示・復旧も 7.9〜8.3秒から 3.6〜3.9秒になった。E-7 の前の main（`959c0bf`、約17.7秒）から見ると約5分の1。
- 開く（表示なし）は 54.9〜56.7 ms から 57.2〜59.3 ms で、約2.5 ms大きい。E-7 と同じく、開くだけでは描画しないので変えた経路を通らない。4k-20 の開くは 1048〜1051 ms と 1033〜1069 ms。
- メモリ：Worker再起動後の WASM は 23.0 MiB から 23.2 MiB。GPU計上値は203.5 MiBで変わらない。
- ラスターの4 fixture とストレス文書は、中央値の範囲が重なるか、差が2 ms未満だった（表は省略）。p95 が上がったのは fx の小さなストロークの確定（等倍）だけで、28.2 ms から 35.2 ms。中央値は 23.9〜26.5 ms と 22.5〜24.3 ms で変わらない。修正前の2回目にも最大37.5 msのサンプルがあり、尾の揺れとみなす。
- GPU 予算超過のエラーは、6回のどれでも起きなかった。

B1〜B8（`e10-bench-{main,fix}-r1〜r3`）。ラスターだけの文書なので、修正した経路は通らない。

| B1〜B8 | 修正前（3回） | 修正後（3回） |
|---|---|---|
| C2 GPUのコールド表示 ms | 282 / 285 / 285 | 280 / 298 / 277 |
| C3 CPU+GPUのコールド表示 ms | 284 / 285 / 283 | 280 / 296 / 278 |
| B1 GPU処理完了フレーム p95 ms | 5.2 / 5.4 / 6.1 | 4.5 / 4.4 / 4.6 |
| B2 ズーム ms | 2.7 / 3.3 / 2.8 | 3.1 / 3.8 / 4.9 |
| B3 連続入力から表示 p95 ms | 18.6 / 18.4 / 19.7 | 15.5 / 18.6 / 23.2 |
| B4 全面のUndo ms | 3.2 / 3.0 / 3.3 | 3.0 / 3.0 / 3.1 |
| B5 保存 ms | 21.5 / 21.5 / 21.5 | 22.0 / 21.6 / 21.3 |
| B5 開く ms | 1063 / 1063 / 1061 | 1079 / 1056 / 1056 |
| B6 GPU計上値の最大 MiB | 493（3回とも） | 493（3回とも） |
| B7 Worker再起動から初回描画 ms | 1325 / 1399 / 1441 | 1387 / 1371 / 1370 |
| B8 小さなストロークの確定から再描画（等倍）ms | 9.2 / 9.1 / 9.8 | 9.2 / 8.8 / 9.0 |
| B8 同（全体）ms | 11.3 / 11.0 / 12.6 | 11.1 / 11.0 / 11.4 |

B1 の p95 は修正後のほうが3回とも小さい（5.2〜6.1 msから4.4〜4.6 ms）。改善の向きの差も、ラスターの経路を変えていないので揺れとみなす。B2 の4.9 msと B3 の23.2 msは、それぞれ1回だけ。B2 は E-2 の main で最大4.6 ms、B3 は E-2 の修正後で最大21.7 msがあり、ラスターの経路は変えていないので、揺れとみなす。

生ログ：`e9-parity`・`e9-perf-try`（試行）、`e10-perf-{main,fix}-r1〜r3`・`e10-bench-{main,fix}-r1〜r3`（比較）、`e11-functional-{metal,swiftshader}`・`e11-perf-try`・`e11-check-native.log`（検証）。

#### 検証

2026-11-14、上と同じ環境で、`9a0de0f`（WASM `3b9eca6f…`）を実行。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `sh scripts/check.sh native` | 0 | 342件成功（新しい試験1件を含む） |
| `cargo test -p alto-gpu` | 0 | 143件成功 |
| `node scripts/verify-browser.mjs --functional`（Metal） | 0 | 9群・341件成功（parity 164件、crash 54件を含む） |
| 同上、`ALTO_BROWSER_GPU=swiftshader` | 0 | 9群・341件成功 |
| `ALTO_PERF_FIXTURES=text-vector node scripts/verify-browser.mjs --suite=perf`（試行1回） | 0 | fit のコールド表示 3715〜3732 ms、等倍 462〜463 ms |

opencode（deepseek-v4.1-flash-free）に、近道の根拠と差分を渡した。挙がった3点のうち2点を直した。
- 「結果を求めたときと使うときで、分割数の上限を別々に渡せる」：今の呼び出しはどちらも `CURVE_LIMIT` だが、食い違えば外側の結果が誤る。上限を `Prepared` に持つようにした。
- 「座標の上限しか確かめていない」：正規化は最小の点を引くので座標は0以上だが、それに頼らないよう下限も確かめるようにした。
- 直さなかった1点：「外側の結果を `whole_rows` が真の状態で求めている」。`flatten` は `whole_rows` を受け取らず、外側の結果は、従来の外側の判定と同じ「何も保存しない」たどり方で求めている。
