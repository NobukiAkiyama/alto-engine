# エンジンの修正・検証：結果

計画：[2026-11-09-engine-hardening-plan.md](../design/2026-11-09-engine-hardening-plan.md)。

| 段階 | 状態 | 実行コミット | 環境 | 記録・PR |
|---|---|---|---|---|
| A | 成功 | `0e6f491` | 下記 | [A](#a基準となる検証) |
| B | 成功 | `1b5ed04` | 下記 | [B](#bcpu合成経路の削除) |
| C | 成功 | 下記 | 下記 | [C](#c書き出しの契約) |
| D | 成功（7/7行。2行目は部分検証） | 下記 | 下記 | [D](#d保存復旧の障害試験) |
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
