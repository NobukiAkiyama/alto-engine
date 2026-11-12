# 検証の残り

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 未検証の環境と入力を、自動または再現可能な手順で確かめる。

**Architecture:** CI は macos-15 の Chrome / SwiftShader と lavapipe、`Apple Paravirtual device` の Metal。性能は CI の合否条件ではない。`alto-doc` に proptest はあるが、cargo-fuzz は未導入。

## QA-1 ファジング

**対象。** 外部から来るバイト列の全入口。

| target | 入口 |
|---|---|
| `package` | `alto_store::decode_package`、`read_entries` |
| `recovery` | `decode_recovery_entries` |
| `snapshot` | スナップショット JSON の読み込み（`open_snapshot`）|
| `commit` | コミット JSON の読み込み |
| `svg` | `crates/alto-engine/src/svg.rs` |
| `font` | `crates/alto-text/src/font.rs` |
| `path` | SVG パスデータの解析（`alto-vector`）|
| `journal` | ジャーナルのレコード解析 |

**配置。** `fuzz/` を workspace の外に置く（nightly が要るため、`rust-toolchain.toml` の固定を崩さない）。seed は `tests/fixtures/*.alto` と `verification/sample*.alto`。

**判定。** パニック、タイムアウト（1入力 10 秒）、メモリ 2 GiB 超えを失敗とする。WASM のパニックはインスタンスを失うので、読み込み経路のパニックは欠陥として扱う。

**CI。** PR では実行しない。週1回の `schedule` ジョブで各 target を 10 分、コーパスは artifact に保存する。クラッシュの再現入力は `tests/fixtures/fuzz/` に追加し、通常の試験で読む。

**既知の不安。** 読み込み以外も含む非テストの `unwrap`、`expect` が主要 crate に約270箇所ある（機械的な数で、未監査）。設計書は Op 適用の経路で `unwrap` を禁止している。ファジングの結果と合わせて監査する。

## QA-2 Safari、Firefox、Edge

**手順。** `scripts/verify-browser.mjs` は Chrome の前提。Playwright の `firefox`、`webkit`、`msedge` のチャネルを選べるよう分離する。

- 最初の結果は機能の有無の表（WebGPU、Worker 内 OffscreenCanvas、OPFS 同期ハンドル、File System Access、Web Locks、`createImageBitmap` のオプション）にする。
- SwiftShader はない環境があるので、parity は別の GPU の結果として記録する。全バイト一致が崩れた場合は、3実装の欠陥か、ブラウザの実装差かを切り分ける。
- フォールバック：Worker から Worker を起動できない環境の Storage Worker のフォールバック（`MILESTONES` 17）が他のブラウザで働くこと。

**判定。** D4 の決定の材料にする。正式対応の一覧を README に書く。

## QA-3 別 GPU の実機

**手順。** 現状の Metal は `Apple Paravirtual device` と、この Mac の実測。対象：NVIDIA / AMD / Intel の Windows、実機の Apple GPU、Android の Adreno / Mali は Chrome が WebGPU を出す範囲で。

- `gpu-parity` の手動実行（`gpu-lifecycle-<OS>` の workflow の形）を self-hosted で実行する。
- 記録：アダプタ名、parity の結果、B1〜B7、`GPU process exited` の回数。`doc/verification/GPU-MATRIX.md` に追記する。

**判定。** parity の不一致はリリースを止める欠陥として扱う。全バイト一致の保証範囲を README に書く。

## QA-4 長時間運用

**手順。** `tests/e2e/soak.js`：決まった乱数シードで編集（ブラシ、Undo、変形、効果、コミット、保存）を4時間続け、1分ごとに記録する。

- 判定：GPU 計上値と WASM メモリが、Undo の上限に達した後に増え続けないこと。B1 と B3 の p95 が初期から 20% 以上悪化しないこと。ジャーナルのサイズが保存で切り詰められること。
- グリフアトラスとタイルアトラスの圧迫（多数のフォントサイズ、多数の入力タイル）で、描画が止まらないこと。
- 最後に保存して開き直し、全タイルが一致すること。
- 実制作での体感と電力は人の評価が要る。別に記録する。

## QA-5 タブ強制終了

**手順。** CDP の `Target.closeTarget` と `Page.crash` で、次の時点でタブを落とし、再起動して復旧を確かめる。

| 時点 | 期待 |
|---|---|
| ストローク中 | 確定済みまで復旧。未確定は失う（STO-7 まで） |
| ジャーナル追記中 | CRC で末尾を除外 |
| チェックポイント書き込み中 | 前の世代へ戻る |
| `.altoproj` の保存中（manifest の前） | 既存の版が読める |
| OPFS の実 GC の削除中 | 再試行で全 root の bytes / hash が保たれる |
| マージの確定中 | 文書だけが変わった状態にならない |

現状の GC 中断は削除を失敗させた再現で、実際のタブ強制終了は試していない。

## 実装タスク

### Task QA-1: ファザー

**Files:** Create `fuzz/Cargo.toml`、`fuzz/fuzz_targets/*.rs`、`.github/workflows/fuzz.yml`。

- [x] `package` と `svg` の2 target を作り、`cargo +nightly fuzz run package -- -max_total_time=60` が動くことを確認。
- [x] 残りの target を追加。
- [x] 検出したクラッシュごとに再現入力を `tests/fixtures/fuzz/` へ置き、試験 `fuzz_regressions_do_not_panic` を追加して修正。
- [x] 非テストの `unwrap` の監査結果を `doc/verification/UNWRAP-AUDIT.md` へ記録。
- [ ] PR。

結果：[UNWRAP-AUDIT.md](../../verification/UNWRAP-AUDIT.md)。3件を修正。

### Task QA-5: 強制終了

**Files:** Create `tests/e2e/crash-tests.js`。Modify `scripts/verify-browser.mjs`。

- [ ] 上表の各行のブラウザ試験を書く。CDP で落とす時点は、SDK の `testing` の出口に待機点を足して決める。
- [ ] CI の機能検証に組み込み、10 回の連続実行で安定を確認。
- [ ] 結果を記録、PR。

進行中：「版のマージ確定中」は計画の後・確定後の保留・記録の途中・durable後の4時点で完了（[D-6](../../verification/ENGINE-HARDENING-RESULTS.md#d-6版のマージ確定中)）。「OPFSのGC削除中」はmanifest確定の前・書き込み途中・1件目の削除後・完了の記録の前の4時点で完了（[D-5](../../verification/ENGINE-HARDENING-RESULTS.md#d-5opfsのgc削除中)）。「`.altoproj` の保存中」はmanifest確定の前・書き込み途中・後の3時点で完了（[D-4](../../verification/ENGINE-HARDENING-RESULTS.md#d-4altoproj-保存のmanifest確定前後)）。「チェックポイント書き込み中」は3時点（pack書き込み後、ジャーナル書き込み途中、ジャーナル確定後）で完了し、版の確定後に編集がないと復旧が提示されない欠陥を直した（[D-3](../../verification/ENGINE-HARDENING-RESULTS.md#d-3チェックポイント切替中)）。「ストローク中」を3時点に分けた行は完了（[ENGINE-HARDENING-RESULTS](../../verification/ENGINE-HARDENING-RESULTS.md#d-1ストロークの3時点)）。残りの行は計画の段階Dで1行ずつ進める。

### Task QA-2: ブラウザの分離

**Files:** Modify `scripts/verify-browser.mjs`。Create `doc/verification/BROWSER-MATRIX.md`。

- [ ] `--browser chromium|firefox|webkit|msedge` を追加する。Chromium の既定の動作を変えない試験。
- [ ] 機能の有無の表を実行で作る。
- [ ] 動く範囲で機能試験を実行し、記録する。

### Task QA-3 / QA-4

- [ ] QA-3：手元にある GPU から順に記録する。無い環境は「未検証」のまま残す。
- [ ] QA-4：`tests/e2e/soak.js` を実装し、4時間を1回記録する。

## Review Focus

- ファザーの seed に実データの個人情報を入れないこと。
- 週次の CI が CI 時間の予算を超えないこと。
- 強制終了の試験が不安定なときに、再実行で隠さないこと。
- 他のブラウザで出た差を、SwiftShader の結果で上書きしないこと。
- 長時間試験のシードと文書を再現できる形で記録すること。
