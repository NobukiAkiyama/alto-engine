# 検証の残り

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 未検証の環境と入力を、自動または再現可能な手順で確かめる。

**Architecture:** CI は macos-15 の Chrome（SwiftShader）の `wasm-browser`、ubuntu-24.04 の lavapipe の `gpu-parity`、macOS ランナーの Metal（`Apple Paravirtual device`）。手動で動かす `gpu-lifecycle` もある。性能は CI の合否条件ではない。ファジング（QA-1）は `fuzz/` に実装済みで、週1回の `fuzz.yml` が回す。長時間試験（QA-4）の `tests/e2e/soak.js` と強制終了試験（QA-5）の `tests/e2e/crash-tests.js` も実装済み。結果は [ENGINE-HARDENING-RESULTS.md](../../verification/ENGINE-HARDENING-RESULTS.md) と [堅牢化計画](../2026-11-09-engine-hardening-plan.md) にある。**未実施は QA-2（Chrome 以外のブラウザ）と QA-3（Apple M5 以外の実 GPU）の2つ**で、QA-1、QA-4、QA-5 には残作業が小さく残る。

実装済みの3項目（QA-1、QA-4、QA-5）は、証拠と残りの作業だけを書く。

## QA-1 ファジング

### 現状

実装済み（main `0e6f491`、2026-11-08）。

- `fuzz/`：workspace の外に置く cargo-fuzz の crate（`fuzz/Cargo.toml`）。nightly が要るため、`rust-toolchain.toml` の固定は崩していない。8 target を `fuzz/fuzz_targets/` に持つ。
- 実行：`fuzz/run-all.sh`。1入力 10 秒（`-timeout=10`）、メモリ 2 GiB（`-rss_limit_mb=2048`）を失敗とする。`TARGETS` で対象を絞れる。seed は `tests/fixtures/*.alto`、`verification/*.alto`、テスト用フォント（`tests/fonts/`）と、`fuzz/seeds/<target>/`。
- CI：`.github/workflows/fuzz.yml`。PR では動かない。`schedule`（月曜 03:17 UTC）と workflow_dispatch（既定 600 秒）で、target ごとのジョブが dated nightly（`nightly-2026-11-08`）と cargo-fuzz 0.13.2 で動く。コーパスは `actions/cache` に積み、artifact `fuzz-corpus-<target>` にも保存する。クラッシュは artifact `fuzz-crash-<target>` に上がる。
- 回帰：クラッシュの再現入力は `tests/fixtures/fuzz/<target>/` に置き、`crates/alto-engine/tests/fuzz_regressions.rs` の `fuzz_regressions_do_not_panic` が通常の試験で読む。今ある入力は3件（`font` 2、`path` 1）。
- 監査：[UNWRAP-AUDIT.md](../../verification/UNWRAP-AUDIT.md)。製品コードの `unwrap` / `expect` は **71箇所**（旧設計書の「約270」はテストを含む数だった）。外部入力で失敗するものは0箇所。ファジングで見つかった3件（`PathData::from_svg` の巨大な円弧、TTC の面の数、sTypoDescender の符号反転）は修正済み。数えたのは基点 `fe87225` で、F-9 より前。

| target | 入口 | 状態 |
|---|---|---|
| `package` | `alto_store::decode_package`、`Engine::open_package` | 実装済み |
| `recovery` | `alto_store::read_entries` → `decode_recovery_entries` | 実装済み |
| `snapshot` | `Engine::open_snapshot` | 実装済み |
| `commit` | `Commit::parse`、`Commit::parse_history` | 実装済み |
| `svg` | `Engine::prepare_svg_import`（`crates/alto-engine/src/svg.rs`） | 実装済み |
| `font` | `alto_text::inspect`、`FontData::new`（`crates/alto-text/src/font.rs`） | 実装済み |
| `path` | `PathData::from_svg`（`alto-vector`） | 実装済み |
| `journal` | `Engine::replay_record`（`verification/sample-m6.alto` に適用） | 実装済み |

実装済みの target が覆っていない外部入力（`bdf26a9` の時点）：

- フォルダ形式（`.altoproj`）の論理エントリを開く `Engine::open_entries`、`Engine::open_entries_on_demand`、`Engine::folder_plan`。`.altoproj` の読み込みは、どの target も通らない。
- 版を `.alto` の外に持つ薄い `.alto`（manifest format 3、`external-objects.json`）。`package` は読めるが、seed に薄い `.alto` が無い。`open_package_entries` の外部履歴の分岐（`external-objects.json` の文書の根と版の突き合わせ）に届く保証がない。
- 画像タイルを復号するデコード用 Worker の入口 `alto_decode::decode_batch`（`crates/alto-decode/src/lib.rs`）。LZ4 の先頭4バイトが示す大きさで確保する。ファジングしていない。入力の出所はエンジン自身が組んだ枠だが、物体の中身は `.alto` 由来である。
- TypeScript 側のジャーナルの読み取り（`packages/alto-sdk/src/storage-core.ts` のヘッダー・CRC・長さの検査）。強制終了試験は不完全な末尾を3種類（ヘッダー途中、ペイロード途中、CRC 不一致）だけ書き出して確かめる。

### 11-04 からの変化

`git log --oneline b2036ce..bdf26a9` で、`fuzz/`、`fuzz.yml`、`tests/fixtures/fuzz/`、`fuzz_regressions.rs`、UNWRAP-AUDIT は `0e6f491`（QA-1）の1コミットで入った。11-04 の記述のうち次が古い。

- 「cargo-fuzz は未導入」：導入済み。
- 「非テストの `unwrap` が約270箇所」：製品コードで 71 箇所。
- 実装タスクの「PR」の未チェック：取り込み済み。
- F-9（版を `.alto` の外に持つ）で増えた入口は、ファザーに入っていない（上の一覧）。

### 設計

実装済み。残りは次の3つ。

**1. target を2つ足す。**

| target | 入口 | seed |
|---|---|---|
| `folder`（新規） | `alto_store::read_entries` で ZIP を論理エントリにし、`Engine::folder_plan`、`Engine::open_entries`、`Engine::open_entries_on_demand`（`known` は入力から取った物体名の集合）を順に呼ぶ | 小さな `.altoproj` を ZIP にしたもの（新規）。薄い `.alto` |
| `decode`（新規） | `alto_decode::decode_batch` | 枠付けした要求。LZ4 の大きさ接頭辞が実際より大きい物、ハッシュ検査ありの物、R8 と Rgba8 を各1件 |

- `decode` は、接頭辞が示す大きさが巨大なとき確保で落ちるかを見る。落ちるなら欠陥として扱う（デコード用 Worker の例外は D-7 の契約でエンジンを止めず、同じスレッドの復号に切り替わる。同じ入力でその Worker も落ちるかは、ファザーで確かめるまで分からない）。
- `folder` の判定は他の target と同じ（パニック、10 秒、2 GiB）。

**2. 薄い `.alto` の seed を足す。** 版を外に持つ保存で作った薄い `.alto`（manifest format 3）を `tests/fixtures/` に1つ置く（新規）。`run-all.sh` の `package`、`recovery`、`folder` の seed に入れる。`tests/fixtures/*.alto` の既存の6つは変えない。文書に実データの個人情報を入れない（生成した合成画像だけ）。

**3. TypeScript 側のジャーナル読み取りを壊して読む。** ファザーではなく、固定 seed の変異試験にする。`storage-core.ts` のジャーナルのバイト列を、先頭・長さ・CRC・末尾の切り詰めと1ビット反転で壊して読む。読み取りは最初の不正（長さ・CRC・形）で打ち切り、それ以降の完全な記録も捨てる（`storage-core.ts` の読み取りループ）。したがって試験は「拒否か除外」では足りず、**記録数を厳密に検証する**：壊した位置より前の記録数と内容が一致し、位置以降は捨てられることを固定する。先頭以外の記録の途中が壊れて後ろに完全な記録が続く場合に、それを黙って捨ててよいかは設計上の確認点とし（利用者の決定。既定は現状の契約の維持）、試験は現状の挙動を明示して固定する。例外の型が不定、ハング、確保の暴走は失敗。試験は `packages/alto-sdk/test/` に置く（新規。既存の SDK の試験と同じく node:test）。

**運用の規則。** 新しい外部バイト列の入口（ICC、PSD、書き出しの逆読みなど、01、09 の項目）を作る PR は、同じ PR で target と seed を足し、`fuzz_regressions.rs` の `run` に同じ名前の分岐を足す。target を足したら次の4か所をそろえる：`fuzz/Cargo.toml` の `[[bin]]`、`fuzz.yml` の `matrix.target`、`run-all.sh` の seed 配置、`fuzz_regressions.rs`。

**週次ジョブの予算。** 10 ジョブ（8 + 2）× 10 分 + ビルド。各ジョブに `timeout-minutes: 30`。PR の CI 時間には入らない。ジョブを増やす前に、直近の週次の実行時間を確かめる。

**`unwrap` の再監査。** 数え方は [UNWRAP-AUDIT.md](../../verification/UNWRAP-AUDIT.md) の「数え方」のとおり。F-9 と、01〜12 の実装が入った後に同じ数え方で数え直し、外部入力で失敗するものが増えていないかを同じ表に追記する。新しいツールは作らない。

### 形式と通信

`format_version`、`PROTOCOL`、manifest、描画の版には触れない。

ただし、`format_version` を上げる PR は、同じ PR で seed を作り直す。`fuzz/seeds/snapshot/*.json` と `tests/fixtures/*.alto`、`verification/*.alto` は現行の版を名乗るので、古い版のままだと `package` と `snapshot` がバージョン検査で弾かれ、本体の読み込みに届かなくなる。seed の作り直しは「完了条件」に含める。

### 作業手順

実装済み：

- [x] `package` と `svg` の2 target、残りの target、`fuzz.yml`、`run-all.sh`。
- [x] 検出した3件の再現入力を `tests/fixtures/fuzz/` に置き、`fuzz_regressions_do_not_panic` で再生。
- [x] `unwrap` の監査（[UNWRAP-AUDIT.md](../../verification/UNWRAP-AUDIT.md)）。
- [x] PR（`0e6f491`）。

残り：

- [ ] **Task QA-1a: `folder` と `decode` の target。** `fuzz/`、`fuzz.yml`、`run-all.sh`（folder の seed 配置を含む）、`fuzz_regressions.rs` の `run` の分岐をそろえた。ローカルで各60秒と各10分の試験でクラッシュ0（ENGINE-HARDENING-RESULTS.md）。完了条件にある新しい matrix での週次 CI 実行は未確認。
  Create `fuzz/fuzz_targets/folder.rs`（新規）、`fuzz/fuzz_targets/decode.rs`（新規）、`fuzz/seeds/folder/`（新規）、`fuzz/seeds/decode/`（新規）。Modify `fuzz/Cargo.toml`（`alto-decode`、`alto-store` の依存と `[[bin]]`）、`.github/workflows/fuzz.yml`（`matrix.target`）、`fuzz/run-all.sh`、`crates/alto-engine/tests/fuzz_regressions.rs`（`run` に分岐）。
  `cargo fuzz list` に `folder` と `decode` が出ること、`TARGETS="folder decode" sh fuzz/run-all.sh 60` が終わり、各 seed がコーパスに入ったこと（`run-all.sh` は一覧にある target だけを動かすので、`[[bin]]` を忘れると黙って省かれる）。見つけた欠陥は再現入力を `tests/fixtures/fuzz/<target>/` に置いて直す（`fuzz_regressions_do_not_panic` がその target を再生する）。
- [x] **Task QA-1b: 薄い `.alto` の seed。** `tests/fixtures/thin-versions.alto`（manifest format 3、version-only object 1件）を生成器 `create-thin-versions-sample.rs` で作った。`run-all.sh` は `package` と `recovery` の seed に入れる（外部履歴の分岐は `open_package_entries` 経路で到達する）。`folder` の seed にも入れるが、folder の `decode_folder` は外部履歴を持つ入力を `UNSUPPORTED` として拒否するため、folder target では拒否経路の試験になる。
  回帰試験（`fuzz_regressions.rs`）で package 経路の外部履歴分岐に届くことを確認。`package` を60秒実行し（74,747 入力、クラッシュなし）、`cargo fuzz coverage package` で `open_package_entries` の外部履歴分岐（`external.take()` の Some 側、行 607）が 8 回実行されたことを確認した。
- [x] **Task QA-1c: ジャーナル読み取りの変異試験。** `packages/alto-sdk/test/journal-mutation.test.js` で先頭・全位置の切り詰め、各レコードの全ビット反転、長さの異常値、CRC が正しい不正タグを検証。破損より前の記録の内容と数を確認する。
  Create `packages/alto-sdk/test/` の試験（新規）。`pnpm --dir packages/alto-sdk build` の後に `node --test packages/alto-sdk/test/*.test.js`。
- [ ] **Task QA-1d: 再監査。** F-9 後の中間確認は UNWRAP-AUDIT.md に追記済み。01〜12 の全項目の実装後に再監査する。

完了条件：2 target と seed が週次ジョブで動き、1回の週次実行（各10分）でクラッシュ0、または見つかった欠陥がすべて直って回帰入力に入っている。検証コマンド：`sh fuzz/run-all.sh 60`、`cargo test -p alto-engine --test fuzz_regressions`。

### 依存

先に要る項目：なし。

後に続く項目：形式を変える項目（`format_version` を上げる PR は seed を作り直す）と、外部バイト列の入口を足す項目（ICC の解析、PSD の読み込み、書き出しの逆読みなど）。ROADMAP の「QA-1、QA-5 を先に入れる」は満たしている。

## QA-2 Safari、Firefox、Edge

### 現状

**未実施。** 動かした環境は Chrome 155〜157、Mac の内蔵 Chromium 152（Metal と SwiftShader）だけ。[MILESTONES.md](../../MILESTONES.md) の継続課題8に「Chrome/Edge/Firefox/SafariでのM7/M8は未検証」とある。README は ChromeまたはEdge を前提にしている。

- `scripts/verify-browser.mjs`：**Chrome を自分で起動して CDP の WebSocket で操作する。Playwright は使っていない**（依存にも無い）。`CHROME` で実行ファイルを選び、`ALTO_BROWSER_EXPECTED_VERSION` で版を固定できる。起動の引数は Chromium 専用（`--headless=new`、`--enable-unsafe-webgpu`、`--use-angle=metal` など）。`/json/version` から版を読む。アダプタ（vendor、architecture、device、description）は `browser-diagnostics.json` に記録する。
- 機能の有無：SDK の `isSupported`（`packages/alto-sdk/src/browser.ts`）が、`transferControlToOffscreen`、`navigator.gpu` とアダプタ、`navigator.locks`、`navigator.storage.getDirectory` の順に調べ、**最初に欠けた1つだけ**を返す。同期アクセスハンドル、入れ子の Worker、File System Access は調べない。
- Storage Worker のフォールバック：`packages/alto-sdk/src/worker.ts` が `createStorageSession` に `createWorker`（入れ子の Worker）と `loadInline`（同じコードをこのスレッドで動かす）を渡す。Worker を起動できない環境ではインラインに落ち、診断 `diagnostic-storage` に `inline journal: ...` が出る（`MILESTONES` 17）。
- GPU の要求：`crates/alto-gpu/src/renderer/device.rs` が `max_texture_array_layers` を既定より引き上げてデバイスを要求する。アダプタの上限が足りない環境では要求が失敗する。
- 強制終了試験（`tests/e2e/crash-tests.js`）は CDP の `Target.closeTarget` と `Page.crash` に依存する。Chromium 系以外では動かない。
- 表の記録先 `doc/verification/BROWSER-MATRIX.md` は無い（新規）。

### 11-04 からの変化

- 「Playwright の `firefox`、`webkit`、`msedge` のチャネルを選べるよう分離する」：前提が違う。`verify-browser.mjs` は素の CDP で、Playwright を入れる必要が生じる。
- 「`createImageBitmap` のオプション」：SDK は `createImageBitmap(file)` を引数1つで呼ぶだけ（`packages/alto-sdk/src/engine.ts`）。オプションの対応は要らない。表からは外し、代わりに `createImageBitmap(blob)` と `OffscreenCanvas` の2D でのデコードを見る。
- `isSupported` は4項目を返す形になった。表の項目はこれに足りない分を足す。
- `verify-browser.mjs` はその後に `--suite=`、`ALTO_BROWSER_EXPECTED_VERSION`、`ALTO_CRASH_REPEAT` などを持った（コミット `1fa0ac7`、`867b19b` ほか）。

### 設計

**ブラウザごとの手段。** 新しい npm 依存は足さない（案と既定）。

| ブラウザ | 手段 | 理由 |
|---|---|---|
| Edge | `CHROME` に Edge の実行ファイルを渡して `verify-browser.mjs` を動かす。コード変更なしで動く見込み（未確認） | Chromium で CDP が使える。強制終了試験も動く |
| Firefox | 機能の有無のページ（下）を WebDriver BiDi で開いて結果を読む。機能試験は後回し | Firefox は CDP を使えない。BiDi は Node 22 の `fetch` と `WebSocket` で足りる |
| Safari | 同じページを `safaridriver`（WebDriver）で開く。または手で開く | Safari の自動操作は `safaridriver` だけ |
| WebKit（Playwright） | 参考値。Safari の結果とは別の欄に書く | Playwright の WebKit は Safari そのものではない |

利用者の決定が要る点：Playwright を開発依存に入れるか。既定は入れない。入れる場合も、Chromium の既定の動作は変えず、別の入口 `--browser` の値で分ける。

**機能の有無のページ。** `tests/diagnostics/browser-features.html`（新規）と `tests/diagnostics/browser-features.js`（新規）。結果の形：

```json
{
  "browser": { "name": "", "version": "", "userAgent": "", "os": "" },
  "adapter": { "vendor": "", "architecture": "", "device": "", "description": "", "maxTextureArrayLayers": 0, "maxTextureDimension2D": 0 },
  "features": { "<key>": { "ok": true, "detail": "" } }
}
```

`ok` は `true`、`false`、`null`（自動では確かめられない）。キーは次の表。1つが欠けても残りを調べ続ける（`isSupported` と違い、先頭の1つで止まらない）。

| key | 確かめること | 方法 |
|---|---|---|
| `secure-context` | isSecureContext | 自動 |
| `webgpu-adapter` | `navigator.gpu.requestAdapter()` が返る。`adapter.info` と上限を記録する | 自動 |
| `webgpu-device-layers` | アダプタの層数の上限が 64 以上（エンジンは 64 未満を `GpuUnsupported` で拒否する）。予算から決まる層数（`cache_layers` の結果。atlas は 64〜512、display は 32〜256）で `requestDevice` が成功する。アダプタの最大値を要求するのではない | 自動 |
| `offscreen-canvas-main` | `HTMLCanvasElement.transferControlToOffscreen` | 自動 |
| `worker-module` | `new Worker(url, { type: "module" })` が動く | 自動 |
| `worker-offscreen-webgpu` | Worker 内で `OffscreenCanvas.getContext("webgpu")` を `configure` し、1フレームを提示する | 自動 |
| `worker-nested` | Worker から Worker を起動できる（できなければ Storage Worker はインラインに落ちる） | 自動 |
| `opfs-directory` | `navigator.storage.getDirectory()` | 自動 |
| `opfs-sync-handle-worker` | Worker で `createSyncAccessHandle` が使え、書いて読み戻せる | 自動 |
| `opfs-sync-handle-nested` | 入れ子の Worker で同上 | 自動 |
| `web-locks` | `navigator.locks.request`（`ifAvailable`、`shared`、`signal`） | 自動 |
| `fsa-picker` | `showSaveFilePicker`、`showDirectoryPicker` が存在する | 自動（存在のみ）。ダイアログの操作は人の操作が要るので `detail` に「未操作」と書く |
| `image-decode` | `createImageBitmap(blob)` と `OffscreenCanvas` の2Dで PNG を復号し、画素が一致する | 自動 |
| `png-encode` | `OffscreenCanvas.convertToBlob` で PNG を書ける | 自動 |
| `wasm-engine` | 同梱の WASM が instantiate でき、空の文書を開く | 自動（SDK を読む） |
| `pagehide` | `pagehide` が発火する（ジャーナルの書き出しが走る） | 手動 |

**3層に分けて記録する。**

1. 機能の有無の表（上のページ）。全ブラウザで取る。
2. 動く範囲の機能試験（`--functional` の各群）。CDP が使える Edge から。他は動かない群を「動かせない」と書き、成功と書かない。
3. parity（GPU 結果とロック済みの画素の比較）。他の GPU の結果として別に記録する。SwiftShader がない環境があるので、**SwiftShader の結果で上書きしない**。

**parity の不一致の切り分け。** 次の順で調べる。

1. その環境で Rust の CPU 参照の値（`tests/parity/cpu-reference.txt`）と JS の参照（`tests/e2e/cpu-composite.js`）が一致するか。参照同士が食い違うなら、JS エンジンか整数演算の実装の差で、WGSL ではない。
2. 参照が一致して GPU だけが違うなら、不一致のケース名と最初に違う画素を記録する。同じシェーダーを Metal、lavapipe、SwiftShader で通ったものなので、ブラウザの WebGPU 実装またはドライバの差。3実装（Rust、JS、WGSL）のどれかの欠陥かは、WGSL が浮動小数を使っていないこと（共通規則2）と、不一致の画素が `n + 0.5` の境界かどうかで判断する。
3. `tests/parity/v3-browser.txt` などのロック済みの行は、他のブラウザの結果で書き換えない。`ALTO_WRITE_V3_BROWSER` は使わない。

**判定。** D4（公式対応ブラウザ）の決定の材料にする。既定の案は「Chrome と Edge を正式、Safari と Firefox は結果で判断」。README の「ChromeまたはEdge」の記述を、表に基づく正式対応の一覧に直す。

**フォールバックの確認。** 入れ子の Storage Worker はエンジン Worker の中で作られるので、ページの `Worker` を差し替えても届かない。確認は2通りに分ける。(a) `worker-nested` が `false` の実環境で、診断に `inline journal` が出て、保存・ジャーナルの書き込み・復旧が通ること。(b) Chromium では、`verify-browser.mjs` のローカルサーバーが `worker.js` を書き換えて返す仕組み（診断用に既にある）で、Storage Worker の生成を失敗させた版を配り、同じことを確かめる（環境変数で選ぶ。新規）。

### 形式と通信

`format_version`、`PROTOCOL`、manifest、描画の版には触れない。`isSupported` の戻り値（`reason`）を増やす場合は SDK の公開型の変更で、`PROTOCOL` は上げない（Worker との要求ではないため）。変えるなら API 検査の出力が変わるので、同じ PR で README を直す。

### 作業手順

- [ ] **Task QA-2a: 機能の有無のページ。**
  Create `tests/diagnostics/browser-features.html`（新規）、`tests/diagnostics/browser-features.js`（新規）、`scripts/browser-matrix.mjs`（新規。ページを開き、結果を Markdown の行にする。Chromium は CDP、Firefox は BiDi、Safari は WebDriver）。Modify `tests/diagnostics/README.md`（使い方）。
  試験：Chrome で全キーが `ok`（`pagehide` は `null`。`fsa-picker` は存在すれば `ok`、無ければ `false`）。`worker-nested` は、ページ自身が作る Worker の中でコンストラクターを塞いだ場合に `false` になること（エンジンの Worker は対象外）。
- [ ] **Task QA-2b: Edge で `verify-browser.mjs`。**
  Modify `scripts/verify-browser.mjs`（ブラウザ名を記録に加える。Chromium の既定の動作は変えない）。Edge を `CHROME` で指定して `--functional` を動かす。Chrome の結果を変えない試験：既存の `summary.json` の項目が変わらないこと。
- [ ] **Task QA-2c: 表と記録。**
  Create `doc/verification/BROWSER-MATRIX.md`（新規）。ブラウザ、版、OS、GPU、機能の表、動かした群、動かせなかった群とその理由を書く。結果が無い環境は「未検証」のまま残す。
- [ ] **Task QA-2d: フォールバックと README。**
  (a) `worker-nested` が `false` の実環境と、(b) 書き換えた `worker.js`（`scripts/verify-browser.mjs` に環境変数を足す）で、保存・復旧を確かめる。D4 の決定後に README の対応表を直す（決定が出るまで書き換えない）。

完了条件：Chrome と Edge と、手元で動く他のブラウザについて、表が埋まり、動かせない項目が理由つきで残っている。検証コマンド：`node scripts/verify-browser.mjs --functional`（Chrome で変化がないこと）、`node scripts/browser-matrix.mjs`（新規）。

### 依存

先に要る項目：なし。QA-3 の「アダプタ名の記録」に、ここで作る機能の有無のページを使う。

後に続く項目：D4（公式対応ブラウザ。利用者の決定）と、README の対応表。ブラウザごとの差が出れば、効果や描画の項目（02、06、07）の GPU 試験の対象を決める材料になる。

## QA-3 別 GPU の実機

### 現状

**未実施。** 実測は Apple M5（Metal）だけ。

- 記録済み：[METAL-V3-RESULTS-2026-10-31.md](../../verification/METAL-V3-RESULTS-2026-10-31.md)。Apple M5（Chrome のアダプタは `architecture` が `metal-3`）で、ネイティブの parity（v3 の合成 74 ケース、ベクター、ぼかし、テキスト）と Chrome のブラウザ試験が全バイト一致。
- CI：`gpu-parity` ジョブ（`.github/workflows/ci.yml`）が ubuntu-24.04 の lavapipe で `cpu_composite::gpu_parity_tests::gpu_shader_matches_cpu_reference` と v3 の各試験を `ALTO_REQUIRE_GPU_PARITY=1` で動かす。macOS ランナーのネイティブ試験は Metal の仮想 GPU（`Apple Paravirtual device`）。`wasm-browser` は SwiftShader（`ALTO_BROWSER_GPU=swiftshader`）。
- `gpu-lifecycle` は workflow_dispatch の入力 gpu_lifecycle で動く手動ジョブ（`gpu-lifecycle-<OS>` の artifact）。macOS と ubuntu のホストランナーで動かす形で、`self-hosted` のランナーを使う設定はどこにもない。
- ブラウザ試験のアダプタ名：`scripts/verify-browser.mjs` が `adapter.info`（vendor、architecture、device、description）を `browser-diagnostics.json` に書く。ネイティブ側は `gpu pixel parity adapter:` の行に出る。
- GPU プロセスの異常終了：`scripts/gpu-process-crash.mjs` が Chrome の stderr から数え、`summary.json` に参考の行（`advisory`）で足す。ジョブは失敗にしない。
- `--functional` を外すと B1〜B7 の測定が走る（`tests/e2e/browser-tests.js`）。
- 表 `doc/verification/GPU-MATRIX.md` は無い（新規）。

### 11-04 からの変化

- 「現状の Metal は `Apple Paravirtual device` と、この Mac の実測」：CI の Metal は仮想 GPU、実機は Apple M5。実機の結果は METAL-V3-RESULTS にある。
- 「`gpu-parity` の手動実行（`gpu-lifecycle-<OS>` の workflow の形）を self-hosted で」：`gpu-parity` は ubuntu-24.04 の lavapipe を前提にした固定のジョブで、手動入力も self-hosted の設定もない。別 GPU の実行は別の入口を作る必要がある。
- GPU の喪失は堅牢化 D-7 で `GPUDevice.destroy()` により起こして試した。ドライバのリセットなど実機の喪失は未検証のまま（QA-5 に書く）。

### 設計

**対象。** NVIDIA / AMD / Intel の Windows、Apple GPU の実機（M5 以外の世代）、Linux の実 GPU、Android（Adreno / Mali）の Chrome。Android は `verify-browser.mjs` が Chrome を自分で起動する作りなので、既定では「手動の確認のみ」にする（QA-2 の機能の有無のページで WebGPU が出る範囲を見る）。

**記録する項目（1環境1行）。** `doc/verification/GPU-MATRIX.md`（新規）の列：

| 列 | 取り方 |
|---|---|
| OS、ドライバの版 | 手で書く |
| ブラウザと版 | `/json/version` |
| ネイティブのアダプタ名とバックエンド | `gpu pixel parity adapter:` の行 |
| ブラウザのアダプタ | `browser-diagnostics.json` の `adapter` |
| ネイティブ parity | `gpu_shader_matches_cpu_reference` と v3 の試験の結果、不一致のケース数 |
| ブラウザ parity | `--suite=parity` の結果 |
| 機能試験 | `--functional` の群ごとの件数 |
| B1〜B7 | `--functional` を外した実行の値。**記録のみで合否にしない** |
| `GPU process exited` の回数 | `summary.json` の参考の行 |
| 実行コミットと日付 | |

最初の行として、Apple M5（Metal）、Apple Paravirtual device、lavapipe、SwiftShader の既存の結果を書き込む。

**実行の入口。** 新しい `scripts/gpu-matrix.mjs`（新規）が、上の列を集めて Markdown の行を標準出力に出す。Windows でも動くよう `sh` を使わない Node のスクリプトにする。内容：

1. ネイティブ：`ALTO_REQUIRE_GPU_PARITY=1` で、`gpu-parity` ジョブの2つのコマンドを動かす。(a) `cargo test -p alto-gpu --locked --lib cpu_composite::gpu_parity_tests::gpu_shader_matches_cpu_reference -- --exact --nocapture --test-threads=1`。(b) `cargo test -p alto-gpu --locked --lib -- --test-threads=1` に、フィルター `cpu_composite::v3_tests::` の v3_vector_、v3_blur_、v3_raster_blur_、v3_backdrop_blur_、v3_feathered_vector_mask_、v3_adjustment_vector_mask_ と、cpu_composite::text_tests::v3_text_ を渡す。Rust の環境が無いホストでは省略し、「ネイティブ：未実行」と書く。
2. ブラウザ：`ALTO_BROWSER_GPU` を設定せず（実 GPU）、`node scripts/verify-browser.mjs` を `--functional` あり・なしで動かす。WASM と SDK のビルドが無いホストは、CI の `browser-build` artifact を展開して使う。

**CI の入口（案と既定）。** `.github/workflows/gpu-matrix.yml`（新規）を workflow_dispatch だけで作り、`runs-on` を入力にする。self-hosted のランナーを登録するかどうかは利用者の決定。既定は「登録しない。上のスクリプトを実機で手で動かして記録する」。登録する場合、公開リポジトリの PR からは決して動かさない（workflow_dispatch のみ、コード実行の権限を持つ人だけ）。

**判定。** parity の不一致は、リリースを止める欠陥として扱う。不一致があれば、ケース名、画素数、最初の不一致の座標、`n + 0.5` の境界かを記録し、**ロック済みの行（`tests/parity/*.txt`）を書き換えない**（`ALTO_WRITE_PARITY`、`ALTO_WRITE_V3_BROWSER` を使わない）。切り分けは QA-2 の手順に従う。全バイト一致の保証範囲は、README に「確認した（GPU、ドライバ、ブラウザ）の組の一覧」として書き、確認していない組は保証しないと書く（MILESTONES の継続課題4の「すべての GPU を網羅した保証ではない」を一覧に置き換える）。

**測定しないもの。** 速度の合否は決めない。B1〜B7 の値は環境の記録で、別の環境との優劣には使わない。

### 形式と通信

触れない。

### 作業手順

- [ ] **Task QA-3a: 記録の入口。**
  Create `scripts/gpu-matrix.mjs`（新規）、`doc/verification/GPU-MATRIX.md`（新規。既存4行を書く）。
  試験：この Mac（Apple M5）で動かし、METAL-V3-RESULTS の結果と同じ件数・一致になること。ネイティブを省いた実行が「未実行」と書くこと。
- [ ] **Task QA-3b: 手元の GPU から順に記録。**
  手元にある環境から1行ずつ追記する。無い環境は「未検証」のまま残す。不一致があれば欠陥として別の PR で扱い、この表には事実だけ書く。
- [ ] **Task QA-3c: README の保証範囲。**
  表ができた時点で README の記述を一覧に置き換える（QA-2 の対応ブラウザの記述と同じ PR にしない。決定が別のため）。
- [ ] **Task QA-3d（利用者が決めたとき）: self-hosted の手動ジョブ。**
  Create `.github/workflows/gpu-matrix.yml`（新規）。

完了条件：表に実測した環境の行が追記され、無い環境が「未検証」と書かれている。検証コマンド：`node scripts/gpu-matrix.mjs`（新規）、`sh scripts/check.sh native`。

### 依存

先に要る項目：QA-2a（アダプタ名の取り方を共有する。必須ではない）。

後に続く項目：D4（QA-2 と合わせて対応表を決める）。描画の3実装を増やす項目（02、03、06、07 の描画タスク）は、完了時にこの表の環境で parity を取り直す。

## QA-4 長時間運用

### 現状

実装済み。4時間の試験は、**薄い保存の形（`ALTO_SOAK_THIN=1`）で最後まで通った**。

- `tests/e2e/soak.js`（`node scripts/verify-browser.mjs --suite=soak`）：`ALTO_SOAK_MINUTES`（既定 240）、`ALTO_SOAK_SEED`（既定 1）、`ALTO_SOAK_INSPECT=1`（保存のたびに `.alto` の中身を分類）、`ALTO_SOAK_THIN=1`（薄い `.alto` と OPFS の版の置き場で保存）。乱数は mulberry32。操作はブラシ、Undo / Redo、変形、効果、テキスト（6〜240px で多数のサイズ。グリフアトラスに圧力をかける）、表示（全体のパン・ズームで多数の入力タイル）、版の commit、保存。操作ごとに GPU 完了を待つ。
- 1分ごとに WASM メモリ、GPU の計上値、メインスレッドの JS ヒープ、ジャーナルの大きさ、履歴の深さ、B1 / B3 型の p95、OPFS の内訳を記録する。最後に保存して開き直し、`snapshotHash`、タイル、PNG の SHA-256、版の一覧を比べる。報告 `soak-report.json` は seed、全サンプル、保存の記録を持つ。
- 判定の関数 `verdict`：Undo が上限（100）に達してから15分後以降を比べ、最後の4分の1の最大が最初の4分の1の最大を 5% かつ 1 MiB 超えたら増加とする。B1 / B3 は最初の5サンプルの中央値と最後の5サンプルの中央値を比べ、20% 超で悪化。ジャーナルは保存のたびに小さくなったか、GPU 喪失・再起動・エラーの数。
- 結果（[F-2](../../verification/ENGINE-HARDENING-RESULTS.md#f-2長時間試験)、[F-9](../../verification/ENGINE-HARDENING-RESULTS.md#f-9版を-alto-の外に持つ)）：
  - 普通の保存の4時間試験は 140 分で `RESOURCE_LIMIT: package exceeds 512 MiB` になって失敗した（版が持つタイルで `.alto` が増える。漏れではない）。
  - 薄い保存の4時間試験（`thin-soak-240`、seed 1、F-9）は240分まで通った。保存 120 回、失敗 0。開き直した結果は一致（版 24）。WASM は 703 MiB で止まり、GPU の計上値は 204〜208 MiB のまま、B1 は 4.2 → 3.2 ms、B3 は 17.5 → 20.3 ms（20% の悪化に当たらない）。ジャーナルは保存で小さくなった。GPU 喪失・再起動・エラーの通知は0件。
  - 上限を超えた後の振る舞いは `package-limit`（F-3）で確かめた。
- 環境：F-2 は Chrome（headless）、GPU は SwiftShader。

### 11-04 からの変化

- `tests/e2e/soak.js` は新規に作る予定だったが、実装済み（コミット `1fa0ac7`、薄い保存の対応は `1038399`）。
- 「保存で切り詰められる」の判定は実装済みで、70回の保存すべてで通った（F-2）。
- 普通の保存の4時間試験は、上限の 512 MiB に届いて失敗した。それを受けて版を `.alto` の外に持つ設計（F-9）が入り、薄い保存の4時間試験が通った。
- 「実制作での体感と電力」は未検証のまま。

### 設計

実装済み。残りは次の4つ。

1. **普通の保存の形での再試験は、今のところしない。** 今の入力と今のコードでは上限に届く見込み（F の結論）で、実際の使い方でどれだけ近づくかは分かっていない。実制作の入力が得られた時点で、`ALTO_SOAK_THIN` なしで同じ seed を回し直すか決める。
2. **アトラスの圧迫を直接確かめる短い試験。** 4時間試験は多数のサイズのテキストと表示のパン・ズームを含むが、グリフアトラス（1024²の1枚。使い切るとベクター経路に落ちる）とタイルのアトラスを使い切る専用の入力ではない。使い切る入力を作り、描画が止まらず、`GPU_LOST` や `INTERNAL` が出ないことを確かめる。判定条件：使い切った後のフレームが提示され、同じ文書の画素が使い切る前後で変わらない。使い切りの観測は `AtlasStats`（`crates/alto-gpu/src/glyph_atlas.rs`）の `fallback_glyphs`、`vector_runs`、`resets` で行う。全段が固定されて挿入できないとき `evicted` と `resets` は増えないので、この2つだけでは使い切りを示せない。`fallback_glyphs` がその場合に数えられることを実装で確かめる。SDK から読めないなら、`testing` 入口に読み出しを足す（新規。本番の公開 API には足さない）か、Rust 側の試験で同じ入力を確かめる。この試験は4時間かけない（10分以内）。
3. **記録の再現性。** 報告は seed と全サンプルを持つ。文書は `soak.js` の `build` が決定的に作るので、コミットと seed と環境変数（`ALTO_SOAK_*`、`ALTO_BROWSER_GPU`）、Chrome の版、アダプタを記録に書く。生ログは verification/out/（gitignore）に出て、`doc/verification/` には載せない。要約だけを ENGINE-HARDENING-RESULTS に書く現状の運用を続ける。
4. **人の評価。** 実制作での体感と電力は人の評価が要る。別に記録する。この項目の自動化はしない。

### 形式と通信

触れない。`soak.js` が読む SDK の公開 API を他の項目が変えたとき（保存の入口、版の置き場）は、同じ PR で `soak.js` を直す。

### 作業手順

実装済み：

- [x] `tests/e2e/soak.js` の実装、短い試し5回、普通の保存の4時間（140分で失敗）、薄い保存の4時間（F-9）。
- [x] 上限を超えた後の試験 `package-limit`（`ALTO_CRASH_ONLY=package-limit`）。

残り：

- [ ] **Task QA-4a: アトラス圧迫の短い試験。**
  Modify `tests/e2e/soak.js`（または新しい群 `tests/e2e/atlas-pressure.js`（新規））、`scripts/verify-browser.mjs`（`--suite=` に追加）。入力はテキスト層を多数のサイズで並べ、サイズ帯を使い切る。試験名は `atlas pressure keeps drawing`（新規）。上の判定条件を満たすこと。
- [ ] **Task QA-4b: 次の機会の再実行。** 形式や保存の入口が変わる項目（08、09）の完了後に、薄い保存の4時間を seed 1 で1回回し、`verdict` を記録する。保存の入口が変わらなければ回さない。
- [ ] **Task QA-4c: 実制作での評価。** 人が行い、別の文書に記録する。

完了条件：アトラス圧迫の試験が Metal と SwiftShader で通る。検証コマンド：専用の群なら `node scripts/verify-browser.mjs --suite=atlas-pressure`（新規。`ALTO_BROWSER_GPU=swiftshader` でも1回）。`soak.js` に組み込む場合は `--suite=soak` で `ALTO_SOAK_MINUTES=20`。どちらかに決めてから書く。

### 依存

先に要る項目：なし（薄い保存は実装済みの F-9）。

後に続く項目：保存・履歴に触れる項目（08 の履歴の打ち切り、09 のジャーナル・書き出し）は、完了時に QA-4b を判断する。

## QA-5 タブ強制終了

### 現状

実装済み。**CDP でタブを落とす試験は28時点・56ケース（2つの落とし方）。うち27時点は10回連続の成功を記録した**（Metal と SwiftShader。540件）。F-9 で足した `thin-open` は、機能試験の群での成功の記録はあるが、10回連続の記録はない。

- 仕組み：`tests/e2e/crash-tests.js`（Node 側）が CDP でタブを開いて落とし、ページ側は `tests/e2e/crash-page.html`。`Page.crash`（unload を経ない）と `Target.closeTarget`（利用者がタブを閉じるのと同じ）の2つで落とす。`scripts/verify-browser.mjs` が機能試験の後に実行する（`--suite=crash`、`ALTO_CRASH_REPEAT` で反復、`ALTO_CRASH_ONLY=<正規表現>` で時点を絞る）。CI は `wasm-browser` の `crash` 群（`m8,crash`）で反復1回。
- 待機点：SDK の `testing` 入口（`packages/alto-sdk/src/testing.ts`）に `holdStorage`、`partialAppend`、`stopCheckpoint`、`destroyGpu`、`testPanic` などがある。本番の公開 API には足していない。
- 判定：新しいタブで同じ名前空間を開いて復旧し、snapshot hash、`stateHash`、層のタイルマップ、版履歴（DAG と全ブランチ）、`dirty` を比べる。復元が例外なく終わっただけでは合格にしない。試験が失敗を検出できることも、わざと壊して確かめた。

| 時点 | 期待 | 状態 | 記録 |
|---|---|---|---|
| ストローク中（未確定、確定後・durable 前、durable 後）3 | 確定済みまで復旧、未確定は失う | 成功 | [D-1](../../verification/ENGINE-HARDENING-RESULTS.md#d-1ストロークの3時点) |
| ジャーナル追記中（ヘッダー途中、ペイロード途中、CRC 不一致）3 | CRC で末尾を除外し、復旧後の追記も通る | 部分検証 | [D-2](../../verification/ENGINE-HARDENING-RESULTS.md#d-2ジャーナルの不完全な末尾) |
| チェックポイント書き込み中 3 | 前の世代へ戻る、または新しい世代 | 成功（欠陥1件を修正） | [D-3](../../verification/ENGINE-HARDENING-RESULTS.md#d-3チェックポイント切替中) |
| `.altoproj` の保存中（manifest の前、途中、後）3 | 既存の版か新しい版のどちらかが読める | 成功 | [D-4](../../verification/ENGINE-HARDENING-RESULTS.md#d-4altoproj-保存のmanifest確定前後) |
| OPFS の GC の削除中 4 | 再試行で全 root の bytes / hash が保たれる | 成功 | [D-5](../../verification/ENGINE-HARDENING-RESULTS.md#d-5opfsのgc削除中) |
| マージの確定中 4 | 文書だけが変わった状態にならない | 成功 | [D-6](../../verification/ENGINE-HARDENING-RESULTS.md#d-6版のマージ確定中) |
| Worker 停止、GPU 喪失 7（追加） | 契約どおりの状態通知と復旧 | 成功（欠陥4件を修正） | [D-7](../../verification/ENGINE-HARDENING-RESULTS.md#d-7worker停止gpu喪失) |
| 薄い `.alto` を開いた後の復旧 1（`thin-open`、F-9 で追加） | 版の置き場を読んで復旧できる | 成功（10回連続の記録なし） | [F-9](../../verification/ENGINE-HARDENING-RESULTS.md#f-9版を-alto-の外に持つ) |

見つけて直した欠陥は計5件（D-3 に1件：版の確定後に編集がないと復旧が提示されない。D-7 に4件）。

- 未検証として残るもの（記録に明記）：
  - D-2：ブラウザや OS の書き込み命令の最中の強制終了、電源断、書き込み順序。試験は不完全な末尾を書き出してから落とす。
  - D-7：`GPUDevice.destroy()` ではない実機の GPU 喪失（ドライバのリセット、外部 GPU の取り外し）、例外を出さずに終わる Worker。
  - D-6：マージした文書を `.altoproj` へ保存している途中、競合の解決が "theirs" 以外の場合。
  - 確認した環境は Apple M5 の Metal と SwiftShader の Chrome だけ。

### 11-04 からの変化

- `tests/e2e/crash-tests.js` と `crash-page.html` は新規に作る予定だったが、実装済み（コミット `867b19b` から `850063c` まで、段階Dの7行）。
- 「現状の GC 中断は削除を失敗させた再現で、実際のタブ強制終了は試していない」：古い。GC の4時点（manifest 確定の前、書き込みの途中、1件目の削除後、完了の記録の前）をタブの強制終了で試した。
- 「ストローク中」は3時点に分けた。「Worker 停止、GPU 喪失」の行が加わった。
- 10回連続の安定の確認は済んだ（CI は反復1回）。

### 設計

実装済み。残りは、上の「未検証」のうち、ここで試験にできるもの。

1. **実際の書き込み中の強制終了（`append-random-kill`、新規）。** 待機点で止めるのではなく、連続した編集の最中にランダムな時刻で落とす。反復ごとに、seed から決めた長さ（0〜2000 ms の一様）だけ編集を続けて `Page.crash` か `Target.closeTarget` で落とし、新しいタブで復旧する。
   - 判定：復旧した状態が、**編集の列の何番目かの接頭辞**と完全に一致する（snapshot hash を編集ごとに記録しておく）。`durable()` が解決した編集は必ず含まれる。含まれない編集があれば欠陥。
   - 反復数：1回の実行で 50 回（seed 固定。失敗した seed と時刻を出力する）。CI では 10 回（SwiftShader で時間が長い。測ってから決める）。
   - 不安定なときに再実行で隠さない。失敗は seed つきで記録し、再現できなければ「未再現」と書く。
2. **Worker が例外なく終わる場合。** Worker が `close()` で自分を終わらせても、`error` イベントも再起動の合図も出ない。`packages/alto-sdk/src/engine.ts` の再起動は `worker.onerror` のほか `retry()`、`recoverAfterStorageFailure`、bfcache からの復帰でも呼ばれるが、無言の終了を検出して起こす経路はない。したがって現状は検出されない見込みで、試験は先に「今は検出されず、要求が返らない」ことを確かめる欠陥の再現になる。直すなら、要求の無応答を監視して `WORKER_CRASHED` で拒否し再起動する設計を別に書く（SDK の変更。Worker との要求を増やすなら `PROTOCOL` を上げる必要がある。値はその時期の回。00-ROADMAP の番号の表）。判定条件：修正後は `restarting` から `ready` に戻り、確定済みの内容が残る。
3. **実機の GPU 喪失。** `destroyGpu` では起こせない。QA-3 の実機で、ブラウザの GPU プロセスを OS から落とす（`scripts/gpu-process-crash.mjs` が数える種類のクラッシュ）手順を手で試し、結果を GPU-MATRIX に書く。自動化しない。
4. **マージ後の保存と "ours" の競合解決。** D-6 の記録の「試していない」を、`.altoproj` の保存の3時点（D-4）をマージ後の文書で繰り返し、解決を "ours" と明示値にして足す。

STO-7（ステージング退避、未確定ストロークの復元）が入ると、「ストローク中」の期待が変わる（未確定の分も復旧する）。STO-7 の PR は、この表の1行目の期待と試験を同じ PR で直す。

### 形式と通信

触れない。待機点を足す場合は `testing` 入口と `packages/alto-sdk/src/protocol.ts` のテスト用の要求の追加になるが、本番の公開 API と `PROTOCOL` の値は変えない（テスト用の入口は`PROTOCOL`の対象外として扱ってきた。変える場合は上げる必要があるかを PR で確かめる）。

### 作業手順

実装済み：

- [x] 表の各行のブラウザ試験、`testing` の待機点、CI への組み込み、10回連続の確認、結果の記録（D-1〜D-7）。

残り：

- [ ] **Task QA-5a: `append-random-kill`。**
  Modify `tests/e2e/crash-tests.js`、`tests/e2e/crash-page.html`（編集の列と接頭辞ごとの hash の記録）。試験名は `append-random-kill`（新規）。`ALTO_CRASH_ONLY=append-random-kill ALTO_CRASH_REPEAT=10` で Metal と SwiftShader の両方で通ること。わざと durable な編集を捨てる変更を入れて、試験が失敗することを確かめる。
- [ ] **Task QA-5b: Worker の自己終了。**
  Modify `tests/e2e/crash-tests.js`。試験名は `worker-silent-exit`（新規）。最初は再現（検出されないこと）を記録し、検出の設計は別の PR（`packages/alto-sdk/src/engine.ts` を変える）で行う。
- [ ] **Task QA-5c: マージ後の保存と "ours" の解決。**
  Modify `tests/e2e/crash-tests.js`。試験名は `merge-ours-altoproj`（新規）。
- [ ] **Task QA-5d: 実機の GPU 喪失。** QA-3 の実機で手で試して `doc/verification/GPU-MATRIX.md`（新規）に書く。
- [ ] **Task QA-5e: 記録。** ENGINE-HARDENING-RESULTS に追記する。

完了条件：`append-random-kill` と `merge-ours-altoproj` が各2つの落とし方で10回連続成功する。`worker-silent-exit` は、検出の修正が入るまでは「検出されない」ことの再現として記録するだけで、10回連続の条件に入れない（修正後に同じ条件を課す）。検証コマンド：`ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`、`ALTO_BROWSER_GPU=swiftshader ALTO_CRASH_REPEAT=10 node scripts/verify-browser.mjs --suite=crash`、`ALTO_BROWSER_GPU=swiftshader node scripts/verify-browser.mjs --functional`（反復1回）。

### 依存

先に要る項目：なし。QA-5d は QA-3a。

後に続く項目：STO-7（期待を直す）、保存・マージに触れる項目（08、09）。新しい永続化の経路を作る項目は、同じ PR でこの表に行と試験を足す。

## Review Focus

- ファザーの seed に実データの個人情報を入れないこと（薄い `.alto` と `.altoproj` の seed は生成した合成画像だけにする）。
- 週次の CI が CI 時間の予算を超えないこと（target を足すたびに、直近の実行時間を確かめる）。
- 強制終了の試験が不安定なときに、再実行で隠さないこと（失敗した seed と時刻を残す）。
- 他のブラウザで出た差を、SwiftShader の結果で上書きしないこと。ロック済みの `tests/parity/*.txt` の行を他の環境の結果で書き換えないこと。
- 長時間試験のシードと文書を再現できる形で記録すること（コミット、seed、環境変数、Chrome の版、アダプタ）。
- 機能の有無の表で、動かせなかった群を成功と書かないこと。
