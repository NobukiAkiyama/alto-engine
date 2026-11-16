# 実行基盤と性能

基点：main `bdf26a9`（2026-11-17 書き直し）

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 残る性能項目と実行環境の拡張を、測定で採否を決められる形にする。

**Architecture:** Main、Engine Worker、Storage Worker、Decode Worker 群の構成。SharedArrayBuffer なしで全機能が動く（`doc/embedding.md` 第4章は COOP / COEP を要らないとしている）。性能は CI の合否条件ではなく、測定はこの Mac の記録である。速度は約束しない。各項目は、採るか見送るかを測定で決める。

## 注意

- 11-04 版の現状の一部は、その後の変更で解消した。堅牢化 E（性能）と F（長時間運用）で、測定の仕組みと値が入った（`doc/verification/ENGINE-HARDENING-RESULTS.md`）。各項目の最初の手順は、現在の値を測り直すことにする。解消済みなら項目を閉じる。
- 11-04 版が挙げた m5/mod.rs、renderer/m5.rs、FramePlanner、compose_m5 は今の main に無い。M5 の名は 4121d69 で機能名に改められ、レンダラーは 725d6f4 で責務ごとに分けられた。効果付きの1タイルは `Renderer::compose_tiles`（`crates/alto-gpu/src/renderer/tile_renderer/mod.rs`）、可視と先読みの再開可能なジョブは `TileJob`（`crates/alto-gpu/src/renderer/jobs.rs`）である。FramePlanner という型は無く、表示キーの列挙は `Renderer::render`（`crates/alto-gpu/src/renderer/frame.rs`）が行う。
- 11-04 版が「未実装」とした中断可能な先読み（P6）は実装済みで、本書の対象から外した。

## 測定の足場と現在値

測定の道具は2つある。どちらも新しく作らない。

- B1〜B8：`node scripts/verify-browser.mjs --benchmark-only`。`tests/e2e/browser-tests.js` に定義がある。C2（GPU のコールド）、C3（CPU と GPU のコールド）、B2（ズーム）、B3（連続入力から表示）、B5（保存・読込）、B6（メモリ）、B7（Worker 再起動から初回描画）を含む。B1〜B8 の文書は20層の半透明ラスターで、C2 と C3 は物理倍率0.5で見る。
- perf 群：`node scripts/verify-browser.mjs --suite=perf`（`tests/e2e/perf-tests.js`）。固定 fixture（`4k-1`、`4k-20`、`sparse`、`fx`、`text-vector`、`ALTO_PERF_STRESS=1` で `stress-64mpx`）の行を、全サンプルつきで `perf.json` に残す（`ALTO_BROWSER_REPORT_DIR` を指定しない実行では、ファイル名は `rebuild-perf.json`）。コールド表示の「fit」は、文書全体をキャンバスに収める倍率である。

環境は E の記録のとおり（Apple M5 / Metal、Chrome 157 headless、キャンバス 970×468、DPR 1）。値は `doc/verification/ENGINE-HARDENING-RESULTS.md` E-2、E-4、E-10 から写した。最後の測定は `7bdbe92` で、`bdf26a9` ではない。その後に F で保存と packs の経路が変わったので、各項目の最初の手順で測り直す。

| 行 | 値（ms） | 出典 |
|---|---|---|
| B1〜B8 の C2（GPU のコールド） | 276〜297（変更前3回と修正後6回） | E-2 |
| B1〜B8 の C3（CPU と GPU のコールド） | 277〜294 | E-2 |
| B2 ズーム | 2.7〜4.6 | E-2、E-10 |
| B3 連続入力から表示 p95 | 16.2〜22.1 | E-2、E-10 |
| B5 保存 / 開く | 21.1〜22.6 / 1049〜1078 | E-2 |
| B7 Worker 再起動から初回描画 | 1363〜1455 | E-2 |
| `4k-20` の fit コールド表示（GPU） | 618〜626（変更前 636〜668） | E-4 |
| `4k-20` の `.alto` を開く / 復旧 | 1034〜1035 / 1271〜1276 | E-4 |
| `fx` の fit コールド表示（GPU） | 336〜356 | E-4 |
| `stress-64mpx` の fit コールド表示（GPU） | 380〜382（変更前 377〜380） | E-4 |
| `text-vector` の fit コールド表示（GPU） | 2187〜2267（最初の main は約61秒） | E-10 |

読み方の規則。

- p95 は、1回の実行で20サンプル以上の行だけに付ける。コールド表示の行は10サンプル（`stress-64mpx` は3）なので、中央値と最小〜最大だけを使う。比べるときは、変更前と変更後を交互に3回ずつ測り、中央値の範囲が重ならないことを要る。
- メモリは3つを混ぜない。WASM メモリ（`wasmBytes`）、メインスレッドの JS ヒープ（`performance.memory`）、GPU リソースの計上値（`gpuBytes`、`Governor` が数えた確保量）である。GPU の計上値は物理 VRAM の使用量ではなく、本書でも VRAM と書かない。デコードプールの最大値（`decodePeakBytes`）は B6 が WASM に足して 1024 MiB と比べる。Playground は `crossOriginIsolated` が false で、Worker を含む JS ヒープは取れない。
- 現在の余裕。B6 の GPU 計上値は最大 493 MiB で、予算の上限 512 MiB に対し約 18 MiB 余る（`budgetUsed` は 517,734,400）。GPU を追加で確保する案（RUN-4、PERF-1、PERF-4、PERF-7）は、この余りを使い切らないことを完了条件に入れる。
- C1（新規文書、全キャッシュが空の初回）は今の測定に無い。perf 群の「cold」は `resetRenderCache` で GPU のキャッシュ（CPU も捨てる行もある）を空にした再表示で、設計書 `COLD-RENDERING-DESIGN.md` の C2 と C3 にあたる。C4（復旧コールド）は perf 群の `recover` 行、C5（外部ファイル）は `open .alto + first display` 行が近い。

## RUN-1 WASM スレッド、SharedArrayBuffer

### 現状

- 配布の WASM は `scripts/build-wasm-pkg.sh` が `alto-wasm` と `alto-decode` を `wasm32-unknown-unknown` のリリースでビルドし、固定した wasm-bindgen で出力する。atomics は使わない。`rust-toolchain.toml` は 1.99.1 に固定されている（nightly ではない）。
- リリースの WASM は `wasm/SHA256SUMS` と照合する（`scripts/verify-wasm-sums.sh`、`scripts/check-build-metadata.mjs`）。ビルドが2種類になると、この照合と SDK の `prepack`（`packages/alto-sdk/build.mjs`、`dist/pkg/alto_decode_bg.wasm` の存在確認を含む）の対象が増える。
- `doc/embedding.md` 第4章は、COOP / COEP が要らないと約束している。Playground も `crossOriginIsolated` が false である。SAB を使う版は、この約束の外の「速くなる選択肢」にしかならない。
- 並列化の足場は既にある。Decode Worker が `alto-decode` の `decode_batch` で LZ4 と内容アドレスの検査を行う（`packages/alto-sdk/src/decode-worker.ts`）。SAB なしの並列である。
- 遅さの所在（E-5 の3、4）：`.alto` を開く処理は全オブジェクトを復号して照合する（`alto-store` の `decode_entries`）。`4k-20` で約1.03 秒、B5 の開くは 1049〜1078 ms、B7 は 1363〜1455 ms。保存（B5）は 21 ms で、もともと短い。
- MILESTONES 16 の WASM の大きさ（4,093,058 bytes、gzip 1,034,059 bytes）は初期計測の値である。比べるときは現在の `verification/build.json` の値を使う。

### 11-04 からの変化

- 文章の結論（SAB なしで足りるかを最初に測る）は変わらない。
- 11-04 は「B5 と B7 が 20% 以上短縮」を条件にしたが、保存（B5）は 21 ms で、20% は約 4 ms にしかならない。判定に使う行を、開く（B5）、B7、perf 群の `open .alto` と `recover` に絞る。
- 並列化の入口が先に見える。RUN-2（SAB なしの Compute プール）で同じ行が条件を満たせば、RUN-1 は要らない。

### 設計

**先に測る。** 実装の形は測定で決める。

1. RUN-2 の測定（`doc/verification/RUNTIME-PARALLEL-MEASURE.md`（新規））で、SAB なしの並列（Worker へ圧縮タイルを渡し、結果を受け取る）が、開く処理と復旧をどれだけ縮めるかを記録する。
2. 採用する条件：RUN-2 の方式を使った後でも、開く（B5）、B7、perf 群の `open .alto`、`recover` の中央値が、さらに 20% 以上短くなる見込みがあり、WASM が2つになる配布の費用（下）と引き合う。見込みは、SAB 版の試作を作って交互に3回ずつ測って確かめる。
3. 見送る条件：上を満たさない。測定文書だけを残し、ROADMAP の RUN-1 を「見送り」にする。

**採用した場合の形。**

- WASM を2つビルドする。atomics なし（既定）と atomics あり（`+atomics,+bulk-memory,+mutable-globals`、`--shared-memory`、最大メモリを宣言）。atomics 版は nightly と `build-std` を要する。`rust-toolchain.toml` の固定、リリースの再現ビルド、`wasm/SHA256SUMS` と合わなくなるので、最初に現在の状況（stable で足りるか）を再確認する。
- 選ぶ条件：`crossOriginIsolated` が true で SharedArrayBuffer が使えるときだけ atomics 版を読む。それ以外は既存の版で、動作に差が出るのは速度だけにする。同じ入力から同じバイト列を出す（共通規則6）。
- 用途の候補：入力のリングバッファ、LZ4 と BLAKE3 の並列化（Rust 側のスレッドは `rayon` 相当を要る。採否は試作で決める）。
- メモリ：共有メモリは最大値を宣言する。WASM32 の上限 4 GiB と、B6 の「WASM とデコードプールで 1024 MiB」の判定線を超えない。WASM メモリは伸びたあと縮まない（F-2）ので、スレッド数を決めて固定する。
- 配布の費用：WASM が2つになる分のサイズ、`wasm/SHA256SUMS` と `verification/build.json` の行、SDK の `prepack`、リリースの手順（`RELEASING.md`）、CI のビルド時間。

### 形式と通信

- 文書の形式（`format_version`）：変えない。
- `PROTOCOL`：Worker の `init` に利用する版（またはスレッド数）を渡す場合は上げる（要割り当て）。Worker が自分で `crossOriginIsolated` を見て選ぶなら上げない。
- 描画の版：変えない。共通規則1のとおり既存の画素を変えない。
- 公開 API：変えない。`doc/embedding.md` 第4章に、「COOP / COEP は要らない。付けると速くなる場合がある」と1行足す。

### 作業手順

**Files:** Create `doc/verification/RUNTIME-PARALLEL-MEASURE.md`（新規）。RUN-2 と共用。採用した場合に限り、Modify `scripts/build-wasm-pkg.sh`、`packages/alto-sdk/build.mjs`、`packages/alto-sdk/src/worker.ts`、`doc/embedding.md`、`RELEASING.md`、`wasm/SHA256SUMS`。

- [ ] RUN-2 の測定を先に終える（RUN-2 の作業手順）。
- [ ] 試作を別ブランチで作り、`4k-20` と `fx` の `open .alto`、`recover`、B5、B7 を、SAB なしの版と交互に3回ずつ測る。WASM、JS ヒープ、GPU の計上値を別の列に記録する。
- [ ] 判定条件に照らして採否を `RUNTIME-PARALLEL-MEASURE.md` に書く。見送りなら、ここで終える。
- [ ] 採用する場合：ビルドの2種類化、実行時の選択、ブラウザ試験 `the isolated build opens the same document as the plain build`（同じ `.alto` を開いて `snapshotHash` と全タイルが一致）、`without cross-origin isolation the plain build is used`。
- [ ] 完了条件：`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional` が通る。atomics 版は COOP / COEP を付けた配信でも同じ関数試験を通す。

### 依存

- 先に要る：RUN-2（同じ測定文書を共有し、SAB なしの結果が条件になる）。
- 後に続く：なし。STO-1（部分読み込み）が開く処理の量を変えるので、判定の測り直しは STO-1 の後にする。

## RUN-2 Compute Worker

### 現状

- Decode Worker の仕組みがある。`crates/alto-decode/src/lib.rs` の `decode_batch`（`decodeBatch`）が、枠づけした要求を LZ4 展開し、内容アドレスを検査して返す。`packages/alto-sdk/src/decode-worker.ts` がその Worker で、`packages/alto-sdk/src/worker.ts` が `DECODE_BATCH`（16）、`DECODES_IN_FLIGHT_PER_WORKER`（2）、`DECODE_TIMEOUT_MS`（2000）で配る。Rust 側の窓口は `Runtime.takeDecodeRequests` と `provideDecoded`（`crates/alto-wasm/src/lib.rs`）。
- Worker から Worker を起動できない環境（MILESTONES 17 の内蔵 Chromium 152）や、Worker が1つ失敗した場合は、エンジンが同じ復号をその場で行う。この退避は Compute プールでも保つ。
- 候補の処理の場所。`image` の resize は `crates/alto-engine/src/image/resample.rs`（`resize_pixel`、`catmull_rom_pixel`、`area_pixel`）。保存は `crates/alto-store/src/lib.rs`（`encode_parts_fetching`）。開くは同ファイルの `decode_entries`。
- 測定の足場。native は `crates/alto-engine/examples/image_bench.rs`（レイヤー編集）と `crates/alto-engine/examples/save_bench.rs`（保存。400 タイルで中央値 72.5 ms、メモリのピークは `.alto` の 1.01 倍、F-5）。WASM 側の `image` 操作の内訳は無い。
- 保存のメモリは F-5 で `.alto` とほぼ同じ大きさに抑えた。並列化で写しを増やすと、この性質（と `.alto` 512 MiB の上限、F-3）を壊す。

### 11-04 からの変化

- `image.rs` は `image/` ディレクトリになった（`resample.rs`、`pixels.rs` など）。
- 保存の経路が変わった。F-5 で、タイルを1つずつ取り出して zip へ書く `encode_parts_fetching` になり、F-9 で薄い `.alto` の保存が加わった。保存の内訳を測る前提が変わるので、11-04 の native の数値は使えない。
- Decode Worker は11-04 の時点と同じ形で、`DECODE_BATCH` などの定数も変わっていない。

### 設計

**先に測る。** 採否は操作ごとに決める。

1. 測る対象：`resize`（4096×4096 の1層）、保存（`.alto` を書く）、開く（`decode_entries`）、復旧（B7）。各操作で、符号化（LZ4）、ハッシュ、リサンプルの時間と、全体の時間を、native と WASM で3回ずつ測る。
2. 採用する条件（操作ごと）：計算（符号化・ハッシュ・リサンプル）が全体の半分以上を占め、Worker への受け渡しの複製を含めても、中央値の範囲が重ならずに短くなる試作がある。IMG-5（GPU 化）は符号化の比率が 50% 未満の操作を対象にしているので、半分で分けて重ならないようにする。
3. 見送る条件：計算が半分に満たない。または受け渡しの複製が利得を消す。

**採用した場合の形。**

- 既存の Decode Worker プールを「Compute プール」に広げる。ジョブの種類（`decode`、`resize`、`compress`、`hash`）を要求の先頭に置く。`alto-decode` の名前は変えず、種類を足す（モジュールが小さいことが利点なので、エンジン全体は読み込まない）。
- 決定性（共通規則6）。結果はタイルごとに計算し、タイルの鍵の順に集める。完了の順序は結果に影響しない。ハッシュは内容から決まるので、Worker の数や順序で変わらない。
- 上限（共通規則5）。要求あたりのタイル数は `DECODE_BATCH` と同じく16。同時に飛ばす数は Worker あたり2、全体で `navigator.hardwareConcurrency` を超えない数。Worker が `DECODE_TIMEOUT_MS`（2000 ms）を超えて黙ったら、そのジョブはその場の処理に戻す。
- メモリ。圧縮タイルと結果は `ArrayBuffer` の転送で渡し、複製しない。プールの最大値は `decodePeakBytes` に含め、B6 の WASM とデコードプールの合計 1024 MiB の判定を保つ。保存では、1タイルずつ取り出して書く性質（F-5）を保ち、全タイルを先に集めない。

### 形式と通信

- 文書の形式：変えない。書き出す `.alto` のバイト列も変えない（同じ入力から同じバイト列。圧縮の結果は LZ4 の決定的な出力を使う）。
- `PROTOCOL`：変えない。Compute プールとの要求は Engine Worker と Compute Worker の間の内部の枠で、SDK と Worker の間ではない。`setDecodeWorkers` の意味も変えない。診断のために `unstable` へ項目を足す場合だけ、要求が増えるので `PROTOCOL` を上げる（要割り当て）。
- 配布：`alto_decode_bg.wasm` が変わるので、`wasm/SHA256SUMS` と `verification/build.json` を更新する。

### 作業手順

**Files:** Create `doc/verification/RUNTIME-PARALLEL-MEASURE.md`（新規）。採用した場合に限り、Modify `crates/alto-decode/src/lib.rs`、`packages/alto-sdk/src/decode-worker.ts`、`packages/alto-sdk/src/worker.ts`、`crates/alto-wasm/src/lib.rs`。

- [ ] 判定のための測定。`crates/alto-engine/examples/image_bench.rs`、`crates/alto-engine/examples/save_bench.rs` で native の内訳を取る。WASM は perf 群の `PNG export`、`save .alto bytes`、`open .alto` の行と、Worker のプロファイルで取る。結果を `RUNTIME-PARALLEL-MEASURE.md` に表で残す（native と WASM、WASM / JS ヒープ / GPU の計上値は別の列）。
- [ ] 判定条件に照らして、操作ごとの採否を記録する。見送りだけなら、測定文書だけをコミットして終える。
- [ ] 採用する場合：試験 `the compute pool returns the same bytes in any completion order`（結果の順序を入れ替えて同じ `.alto` が出る）、`a silent compute worker falls back to the inline path`、`saving through the pool keeps the peak near the package size`（`save_bench` で 1.1 倍以内）。
- [ ] 完了条件：`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`、`node scripts/verify-browser.mjs --functional`（parity と crash の群を含む）。ネストした Worker が使えない環境でも通ること。

### 依存

- 先に要る：なし。
- 後に続く：RUN-1（この測定を前提に判定する）。IMG-5 は、同じ測定表（符号化の比率）を使う。

## RUN-3 ネイティブ

### 現状

- `alto-engine`、`alto-gpu` などはネイティブでビルドして試験する。CI の `native` ジョブ（`macos-15`）は `sh scripts/check.sh native`（`cargo test --workspace`）を動かし、Metal の実機アダプターで GPU の試験を含む。`gpu-parity` ジョブは Linux の lavapipe で同じ画素の試験を動かす。
- `examples/native-viewer/` は無い。`examples/` は `browser-playground`、`minimal-embed`、`react-embed`、`vector-comparison` だけで、ワークスペースの `members` にも入っていない（`Cargo.toml`）。
- 1フレームの描画に必要な部品は揃っている。`Engine::render_scene(handle)`（`crates/alto-engine/src/lib.rs`）が `RenderScene` を作り、`Engine` は `TileStore` を実装する。`Renderer::render(&scene, &store, view, budget_ms)`（`crates/alto-gpu/src/renderer/frame.rs`）は同期の関数だが、最後に表示面へ present するので、表示面（`wgpu::Surface`）が無いと `Internal` で失敗する（`renderer has no surface`）。WASM の窓口 `crates/alto-wasm/src/lib.rs` が、この組み合わせを呼んでいる。つまり `alto-engine` に render_frame を足さなくても、同期の描画はできる。
- `alto-engine` は `alto-gpu` に依存しない（`crates/alto-engine/Cargo.toml`）。逆に `alto-gpu` が `alto-engine` を dev-dependency に持つ。`alto-engine` に `native` モジュール（新規）を置くには、`alto-engine` が `alto-gpu` と wgpu に依存する必要があり、`alto-git` のような GPU を要らない利用者にも wgpu を持たせる。
- ウィンドウ無しの `Renderer` は `Renderer::headless` で、`#[cfg(test)]` かつ `pub(crate)` である（`crates/alto-gpu/src/renderer/device.rs`）。公開の `Renderer::new` は `wgpu::Surface` を要る。
- 画素の取り出しは `Renderer::read_tiles`（`crates/alto-gpu/src/renderer/tile_read.rs`）。表示タイルを 512 MiB までまとめて読む。
- 試料は `verification/sample-m78.alto`。画素の基準は `tests/parity/` にあるが、`sample-m78.alto` そのものの基準行は無い。

### 11-04 からの変化

- `alto-gpu` のレンダラーが責務ごとのファイルに分かれた（725d6f4）。
- 11-04 は「render_frame の同期版を `alto-engine` の公開 API に足す必要があるか」を問うていた。上のとおり `Renderer::render` が既に同期なので、足りないのは `alto-gpu` 側の窓口（ウィンドウ無しの生成）で、`alto-engine` ではない。
- 11-04 の「`alto-engine` の `native` モジュール」案は、依存の向きの問題がある。案として残し、既定にはしない。

### 設計

**案。** 利用者の決定が要る。

| 案 | 内容 | 費用 |
|---|---|---|
| A（既定） | `examples/native-viewer` の中だけで `Engine` と `Renderer` をつなぐ。`alto-engine` の公開 API は増やさない。`alto-gpu` にはウィンドウ無しの生成を1つだけ公開する（下） | 小さい。`alto-gpu` の公開が1つ増える。SemVer の約束は決めない |
| B | 新しい crate `alto-native`（新規）に `Viewer`（`open`、`render_frame`、`save`）を置く。これを SemVer の対象にする | 中。crate が1つ増え、`Cargo.toml` の公開手順が要る |
| C | `alto-engine` に `native` モジュール（`Engine`、`DocHandle`、`render_frame`（新規）、`save`、`open`）を置く | 大。`alto-engine` が wgpu を引く。`alto-git` と WASM の依存が変わる |

既定は A。crates.io への公開は最初のリリースの手順で決める（本項目では決めない）。

**A の形。**

- `Renderer::new_headless(instance, width, height, budget_mb)`（新規、`alto-gpu`）。今の `headless` は試験専用（`#[cfg(test)]`、`pub(crate)`）なので、公開の入口を作る。表示面を持たないので `Renderer::render` は呼べない（present が必ず失敗する）。使えるのは `read_tiles` と、読み戻しを使う操作だけである。WASM の構築には影響しない。
- `examples/native-viewer`（新規）は winit と wgpu で `.alto` を開き、パン（ドラッグ）とズーム（ホイール）を行う。入力は `Viewport`（中心、倍率、回転）の更新だけで、描画は `Renderer::render` の予算つき呼び出しを、完成まで繰り返す。
- 画素の確かめは、ウィンドウを使わない関数（`render_sample_tiles`（新規））で行う。`verification/sample-m78.alto` を `Engine::open` で開き、`render_scene` で作った `RenderScene` と、`Engine`（`TileStore`）を `read_tiles` に渡し、指定したレベルの表示タイルを直接作って読む。`Renderer::render` と present の経路は、ウィンドウつきの例でだけ動く（CI では確かめない）。

**画素の基準。** 既存の行は変えない（共通規則1）。新しい行を `tests/parity/native-viewer.txt`（新規）に足す。形式は `v3-browser.txt` と同じ FNV-1a 64 の16進に、名前をつけた行。基準の値は、最初に既存の GPU 試験の経路（`crates/alto-gpu/src/cpu_composite/renderer_runs.rs`）の出力と一致することを確かめてから書き込む。Metal と lavapipe の両方で一致すること。

**上限。** 読み戻しは既存の `read_tiles` の 512 MiB を超えない。ウィンドウの大きさと予算（`budget_mb`、64 以上）は既存の検査に任せる。

### 形式と通信

- 文書の形式、`PROTOCOL`：変えない。
- 描画の版：変えない。
- 公開 API：案 A は `alto-gpu` に `Renderer::new_headless`（新規）を足すだけ。SDK の API は変えない。案 B または C を選ぶときは、公開する型を SemVer の対象として `doc/` に明記する。

### 作業手順

**Files:** Create `examples/native-viewer/`（新規。`Cargo.toml`、`src/main.rs`、`src/lib.rs`）、`tests/parity/native-viewer.txt`（新規）。Modify `Cargo.toml`（`members` に追加）、`crates/alto-gpu/src/renderer/device.rs`（`new_headless`）、`Cargo.lock`。

- [ ] 設計の案を決める。既定は A。
- [ ] `Renderer::new_headless` を足し、既存の `headless` を呼ぶ形に寄せる。試験 `a headless renderer reads the same tiles as the test renderer`。公開 API が1つ増えるので、`alto-gpu` の公開の扱い（SemVer の対象か）を PR に書く。
- [ ] `examples/native-viewer` を作る。依存の winit を `Cargo.lock` に固定する（`--locked`）。
- [ ] 試験 `native_viewer_renders_the_sample`（`examples/native-viewer/src/lib.rs`）：試料を描いたタイルの FNV-1a 64 が `tests/parity/native-viewer.txt`（新規）の行と一致する。GPU が無い環境では、既存の GPU 試験と同じ扱いで飛ばす。
- [ ] 完了条件：`sh scripts/check.sh native`（clippy を含む）。`cargo run -p native-viewer -- verification/sample-m78.alto` が手元で開く（CI では動かさない）。

### 依存

- 先に要る：なし。
- 後に続く：RUN-5（ネイティブ側のテクスチャ共有は、この例の上に載せる）。

## RUN-4 readback プール2本化

### 現状

- 読み戻しのプールは1デバイスに1本である（`crates/alto-gpu/src/renderer/readback.rs` の `Pool`）。大きさは `capacity(budget)` = 8 MiB と予算の 1/16 の小さい方を、タイル（256 KiB）単位に切り下げた値。予算 512 MiB で 32 タイル、64 MiB で 16 タイル。ストローク確定と export が同じプールを共有し、`busy` の間に別の取得が来ると待たずに `Conflict` で断られる（`doc/embedding.md` 第4章）。
- 状態は `ReadbackStats`（`capacity`、`batches`、`completed`、`mapped`）として tick に載る。待ち時間の計測は無い。
- B3（連続入力から表示）の p95 は 16.2〜22.1 ms で、判定線 33.4 ms に届いていない（E-2、E-10）。`B6` の全面ストロークの確定時間（`finishMs`）は記録されている。
- 予算の余裕が小さい。B6 の GPU 計上値は最大 493 MiB で、上限 512 MiB まで約 18 MiB。プールをもう1本、8 MiB で確保する余地はあるが、PERF-1、PERF-4、PERF-7 と取り合う。

### 11-04 からの変化

- ファイルの場所（`readback.rs`）と大きさの式は変わらない。850063c（D-7）で GPU 喪失時の復旧が直った。2本化の故障試験は、この修正後の振る舞いを基準にする。
- 堅牢化 E で B3 と B6 の値が記録された。B3 は判定線の内側にあり、読み戻しの待ちが問題になっている証拠は無い。
- 11-04 の判定条件「B3 の連続描画で読み戻しの待ちが観測された場合」は、観測する計測が無いので判定できない。計測を先に足す。

### 設計

**先に測る。**

1. `Pool` に計測（新規）を足す。今のプールは待たない。`Pool::acquire` は `busy` のとき即座に `Conflict`（`readback pool busy`）を返し、`next_batch` に `busy` の待ちは無い。したがって測るのは、`acquire` が `busy` で断られた回数と、バッチごとのコピー提出から map 完了までの時間の合計で、`ReadbackStats` に足す。診断のときだけ有効にし、本番のフレーム時間を増やさない（時刻の読み取りだけ。確保しない）。
2. B3 と B6 の全面ストローク確定で、待ちの合計が確定時間（`finishMs`）に占める割合を3回ずつ記録する。
3. 採用する条件：全面ストローク確定の時間の半分以上が、バッチのコピー提出から map 完了までの時間（2本にすれば重ねられる部分）で、または `busy` の拒否が実際の操作（export と確定の重なり）で観測され、B3 の p95 が判定線に近づく（33.4 ms の8割以上）か、確定時間の短縮が目標になる。
4. 見送る条件：待ちが半分に満たない。現状の B3 の値では見送りが見込まれる。測定結果を書いて閉じる。

**採用した場合の形。**

- プールを2本にし、本数ぶんを先に予約する（`Governor::shrink_readback` と同じ予約の規則）。1本あたりの大きさを半分にして合計を変えない案を既定にし、合計を増やす案は B6 の 493 MiB に 8 MiB 足してよいかを測って決める。
- copy と map を並行にする。バッチは交互に使い、完了の順序は提出の順序に揃える（呼び出し側から見える結果の順序を変えない）。
- 故障試験は1本の場合と同じものを2本で繰り返す：ストロークの取り消し、GPU 喪失、確保失敗、`FinishControl` による中断（`doc/design/2026-10-27-presentation-readback-guarantees.md` 第5章、第10章）。1本が確保に失敗したら1本に縮み、`RESOURCE_LIMIT` にはしない。

### 形式と通信

- 文書の形式、描画の版：変えない。
- `PROTOCOL`：変えない。計測値は tick の数値列に足す（`FRAME_FIELDS`（`crates/alto-wasm/src/lib.rs`）の末尾に名前を足す。SDK は名前で読む）。
- 公開 API：変えない。

### 作業手順

**Files:** Modify `crates/alto-gpu/src/renderer/readback.rs`、`crates/alto-wasm/src/lib.rs`（tick の名前）、`tests/e2e/browser-tests.js`（B6 の記録）。Create `doc/verification/RUN4-READBACK-MEASURE.md`（新規）。

- [ ] 計測を足す。試験 `readback_counts_busy_rejections_and_map_time`。
- [ ] B3 と B6 を3回ずつ測り、`RUN4-READBACK-MEASURE.md` に表で残す。判定条件で採否を書く。
- [ ] 見送りなら、測定の計測だけをコミットして終える。
- [ ] 採用する場合：試験 `two_pools_return_batches_in_submission_order`、`one_pool_failing_leaves_the_other_working`、`gpu_loss_releases_both_pools`。ブラウザの crash 群（`tests/e2e/crash-tests.js`）に、2本の状態での GPU 喪失を足す。
- [ ] 完了条件：`sh scripts/check.sh native`、`node scripts/verify-browser.mjs --functional`、B6 の GPU 計上値が 512 MiB の判定内。

### 依存

- 先に要る：なし。
- 後に続く：VEC-4（vello の readback の整理と競合しうる）。RUN-5 の export の待ちの契約。

## RUN-5 他 queue、外部 GPU API との所有権移譲

### 現状

- レンダラーは1つの `wgpu::Queue` に閉じ、読み戻しは1本のプールを通る。`Document.exportImage`（`packages/alto-sdk/src/document.ts`）が、表示タイルを読み戻して PNG か WebP の `Blob` を返す。Worker 側の実装は `exportImage`（`packages/alto-sdk/src/worker.ts`）で、出力は 64,000,000 画素、CPU の操作は 512 MiB を超えたら `INVALID_ARGUMENT` で断る。
- 外部の画像の取り込みは `engine.importImage(file: Blob)`（`packages/alto-sdk/src/engine.ts`）で、テクスチャの取り込みは無い。
- `GPUDevice` は Worker をまたいで共有できない。エンジンの device は Engine Worker 内の `OffscreenCanvas` に結びつく。ホストのページが持つ device のテクスチャを、そのまま渡すことはできない。
- GPU 喪失（`GPU_LOST`）の後の復旧は `recoverGpu`（`crates/alto-wasm/src/lib.rs`）が `Renderer` を作り直す。喪失の前に作ったテクスチャ（のコピー）は、喪失の後は使えない。
- 契約の文書は `doc/embedding.md`。同期の契約（読み取りの完了と所有の移動）はまだ無い。

### 11-04 からの変化

- 新しい決定は無い。`PRESENTATION-READBACK` 第11節は今も「後続」のままである。
- 11-04 の API 名 exportTexture と importTexture は、Web では作れない（上）。名前と形を見直す。

### 設計

**Web。** 利用者の決定が要る点は名前だけである。既定を示す。

- `doc.exportBitmap(options?: ExportImageOptions): Promise<ImageBitmap>`（新規）。`exportImage` と同じ引数（`region`、`scale`）、同じ検査、同じ上限（64M 画素、CPU の操作 512 MiB）。表示タイルを読み戻したストレート・アルファの画素から ImageBitmap を作って転送する。受け取った側が所有し、文書の以降の編集は反映されない（コピーなので、所有の問題は起きない）。
- 取り込みは既存の `engine.importImage` と、それを使うトランザクションに任せる。importTexture は作らない。外部 GPU の結果を受けたいホストは、ImageBitmap を `Blob` にして渡す。
- GPU 喪失中は `GPU_LOST` で断る。喪失の前に作った ImageBitmap はコピーなので有効である。

**ネイティブ。** RUN-3 の例の上で、`wgpu::Texture` の受け渡しを提供する。

- `export_texture(region) -> wgpu::Texture`：読み戻しではなく、レンダラーの queue でコピーして新しいテクスチャを作る。コピーを提出したあと、`Queue::on_submitted_work_done` が完了するまで、受け取る側は読んではいけない。完了の通知は callback 式なので、関数は `(wgpu::Texture, 完了を知らせる oneshot の受信側)` を返す（`alto-gpu` が既に使う `futures-channel` を使う）。契約は「返す時点でコピーは提出済み。読み取りは、受信側が完了してから」。
- 同じ device のテクスチャ共有（コピーなし）は提供しない。ゼロコピーが要る場合は、別の項目で扱う。

**契約の文書。** 実装の前に `doc/embedding.md` に章を足す：誰が所有するか（常に受け取る側）、いつ読めるか（Promise または Future の完了後）、文書が変わったときの扱い（コピーなので反映されない）、GPU 喪失の扱い。

ブラウザ・ドライバー側の間欠的な GPU 喪失そのものは本エンジンで根治できない。復旧と記録（`scripts/gpu-process-crash.mjs`）を維持する。

### 形式と通信

- 文書の形式、描画の版：変えない。
- `PROTOCOL`：`exportBitmap`（新規）の要求を足すので上げる（要割り当て）。既存の `exportImage` は変えない。
- 公開 API：`Document.exportBitmap`（新規）を足す。`pnpm --dir packages/alto-sdk api` で `packages/alto-sdk/api/` の報告を更新する。

### 作業手順

**Files:** Modify `doc/embedding.md`、`packages/alto-sdk/src/document.ts`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/api/`（更新）、`tests/e2e/embed-tests.js`。ネイティブは `examples/native-viewer/`（RUN-3 の新規）。

- [ ] 同期の契約を `doc/embedding.md` に書く。
- [ ] ブラウザ試験 `an exported bitmap holds the composite after later edits`（export の後に編集しても、画素が変わらない）、`an export after GPU loss is refused`（喪失中は `GPU_LOST`。取り込みの `engine.importImage` は SDK 側で画素を作るので、読み戻しも GPU の所有も通らず、この契約の試験にならない）、`an export over the pixel limit is refused`（64M 画素超は `INVALID_ARGUMENT`）。
- [ ] ネイティブ試験 `a shared texture read waits for the queue`（提出済みのコピーが完了する前に読まない）。
- [ ] 実装、PR。
- [ ] 完了条件：`pnpm --dir packages/alto-sdk build`、`api:check`、`node scripts/verify-browser.mjs --functional`、`sh scripts/check.sh native`。

### 依存

- 先に要る：RUN-3（ネイティブ側）、RUN-4 の判定（読み戻しのプールの形が決まってから）。
- 後に続く：なし。

## PERF-1 可視タイルのバッチ化

### 現状

- 可視タイルは `Renderer::render`（`crates/alto-gpu/src/renderer/frame.rs`）が、画面の中心に近い順に1枚ずつ作る。レベル1以上の v3 の親は、子4枚の整数の縮小としてだけ定義されるので（`compose_from_children`、`hierarchy_step`）、子を `TileJob` や直接合成で作ってから縮小する。
- 通常のラスターは `TileJob`（`crates/alto-gpu/src/renderer/jobs.rs`）が、計画（1ステップ1ノードまたは1ソースタイル）、取得・転送、スロットの引き直し、提出の順に、約 1.8 ms（`JOB_SLICE_MS`）ごとに Worker へ制御を返す。固有入力の重複除去は出力タイル1枚の中だけで、画面全体では共有しない（COLD-RENDERING-RESULTS が「フレーム全体の共有バッチは未実装」と記録している）。
- アトラスと表示キャッシュの枚数は予算で決まる（`cache_layers`、`crates/alto-gpu/src/renderer/memory.rs`）。既定の 512 MiB では、アトラス 512 枚、表示キャッシュ 256 枚。
- 効果、マスク、テキスト、ベクターを含む文書は `plain_scene`（`crates/alto-scene/src/damage.rs`）が偽になり、`TileJob` を通らず `compose_tiles` の同期経路を使う。
- 計測の足場：`RenderProfile` の `unique_inputs`（出力タイルごとの固有入力数の合計）、`atlas_hits`、`upload_bytes`、`steps`、`max_step_ms`。
- 各 fixture の入力の共有。`4k-20` は全面の半透明20層で各タイルが別の入力を持ち、`stress-64mpx` は全タイルが異なる。入力の取得と転送を画面全体で共有しても、共有できる入力が少ない文書では利得が出ない。

### 11-04 からの変化

- compose_m5 は `Renderer::compose_tiles` になった。M5 の用語は残らない。
- 11-04 版は設計書 P5 の文面（アトラス容量内でバッチ化）をそのまま写していた。実際には、11-04 より前に可視ジョブ（COLD-RENDERING-RESULTS の P2）が入っており、v3 では親が子からだけ作られる。バッチ化の利得が残るのはレベル0のタイルの提出と入力の共有で、その見込みは測定がなければ分からない。
- 11-04 の判定「B2 の短縮」は使えない。B2 は 2.7〜4.6 ms で、余地が無い。判定は perf 群の「cold GPU first display (fit)」とする（05 の IMG-4 と同じ行）。

### 設計

**先に測る。** 実装の形は測定で決める。

1. perf 群の `4k-20`、`fx`、`sparse`、`stress-64mpx` の fit コールド表示で、`RenderProfile` の `steps`、`commands_ms`、`get_ms`、`write_ms`、`unique_inputs`、`upload_bytes`、`max_step_ms` を記録する。あわせて、画面全体の固有入力数（新規）を数え、「出力タイルごとの固有入力の合計」との差を共有で省ける入力数として記録する。
2. 採用する条件：共有できる入力が出力タイルごとの入力の合計の2割以上ある fixture があり、その fixture の fit コールド表示（中央値の範囲）が、プロトタイプで重ならずに短くなる。
3. 見送る条件：共有できる入力が少ない（`4k-20`、`stress-64mpx` はこの型）。提出の回数を減らすだけの利得なら、`steps` と `max_step_ms` に支配されているかを見て決める。見送るときは測定を記録して閉じる。

**採用した場合の形。**

- 不足している表示タイルを、中心に近い順に列挙し、必要な入力のハッシュを画面全体で重複除去する。バッチの大きさは、アトラスの空き枚数と表示キャッシュの空き枚数から決め、1タイルで容量を超える場合は従来の `TileJob` に戻す。
- バッチの間は、入力のスロットと出力のスロットを保護する（`pyramid_pins` と同じ流儀）。提出の順序は、画面の中心に近い順を保つ（可視タイルの優先が崩れない）。
- 効果付き文書（`plain_scene` が偽）とストローク中は、従来の経路。GPU 喪失の後は、`Renderer` ごと作り直されるので、バッチの状態は残らない。
- 全バイトが v3 の既存の画素と一致する（共通規則1、2）。
- 予算：バッチが一時的に多くのアトラス枚数を保護して、`GPU budget` のエラー（E-5 の1）を再現させない。保護する枚数に上限を置く。

### 形式と通信

- 文書の形式、`PROTOCOL`、描画の版：変えない。診断の数値を足すときは tick の名前の末尾に足す。

### 作業手順

**Files:** Modify `crates/alto-gpu/src/renderer/frame.rs`、`crates/alto-gpu/src/renderer/jobs.rs`、`crates/alto-gpu/src/renderer.rs`（`RenderProfile`）、`crates/alto-wasm/src/lib.rs`（tick の名前）。

- [ ] 現在の `4k-20`、`fx`、`sparse`、`stress-64mpx` の fit コールド表示と `RenderProfile` を3回測り、`doc/verification/COLD-BREAKDOWN.md`（新規）。PERF-3 と共用に記録する。
- [ ] 判定条件で採否を書く。見送りなら、ここで終える。
- [ ] 採用する場合：試験 `a_batch_returns_the_same_bytes_as_single_tile_jobs`（バッチ境界をまたぐ半透明20層、アトラス退避、キャンバス端。全バイト一致）、`a_batch_keeps_the_center_first_order`、`a_batch_never_pins_more_than_the_limit`、`gpu_loss_drops_the_batch`。
- [ ] ブラウザ：parity 群（`node scripts/verify-browser.mjs --suite=parity`）が全て一致、GPU 喪失の crash 群が通る。
- [ ] 完了条件：B2 ではなく、判定に使った行が短縮した記録。短縮がなければマージしない。`sh scripts/check.sh native`、`node scripts/verify-browser.mjs --functional`（Metal と SwiftShader）。

### 依存

- 先に要る：PERF-5（時刻計測）、PERF-2（シーン索引。計画の費用を先に下げる）。
- 後に続く：IMG-4（再測定の前提）、VEC-4（判定は後）。

## PERF-2 シーン索引

### 現状

- 計画は、ノード木を根から辿る。`TileJob::plan_one`（`crates/alto-gpu/src/renderer/jobs.rs`）が1ステップに1ノードまたは1ソースタイルを進め、ノード内のタイルは、並べ替え済みの列を二分探索して引く。直接合成の `Renderer::commands`（`crates/alto-gpu/src/renderer/compose.rs`）も同じように木を辿る。
- 出力タイルごとに木を辿るので、ノード数 N とタイル数 T に対して N × T に比例する。ノードが多い fixture は `text-vector`（ベクター層8枚、125パス、テキスト層24枚）と、層の多い `4k-20`。
- 索引は存在しない。`crates/alto-scene/src/lib.rs` にあるのは `RenderScene`（`RNode` の配列。`RIdx` は `usize`）と、`RenderScene::compile`（`RenderPlan`）である。`RNode::version` は文書全体の `revision` で、ノードごとの値ではない（`Engine::render_scene`）。
- 変更の範囲の計算は `crates/alto-scene/src/damage.rs` の `scene_damage`。差分の更新に使える。
- 測定の足場：`RenderProfile::commands_ms`（可視ジョブでは計画だけの時間）。

### 11-04 からの変化

- 11-04 は索引を `SceneIndex::query(tile) -> impl Iterator<Item = RIdx>` と書いた。名前は今も無く、新規である。
- 11-04 版は計画の探索を線形と書くが、実際は可視ジョブ（COLD-RENDERING-RESULTS の P1/P2）が 11-04 より前に入り、`TileJob` が二分探索でタイル列を引く。残るのはノードの探索で、「ノード数に対して線形」はまだ成り立つ。
- E-10 のプロファイルでは、`text-vector` の fit コールド表示（約 2.2 秒）の中身は、パスのラスタライズ、GPU への転送、128 ビットの掛け算であり、計画の探索は上位に出ていない。ラスターの fixture でも `commands_ms` は支配的でない。採否は測定で決める。

### 設計

**先に測る。**

1. `RenderProfile::commands_ms` を、`text-vector`、`4k-20`、`sparse` の fit コールド表示と、ウォームのパン、小さなストロークの確定で記録し、全体の時間に対する割合を表にする。
2. 採用する条件：どれかの fixture で `commands_ms` が全体の1割以上を占め、かつそれがノード探索に由来する（プロファイルで `plan_one` の自己時間が見える）。
3. 見送る条件：どの行でも1割に満たない。現在の値では見送りが見込まれる。結果を記録して閉じる。

**採用した場合の形。**

- `SceneIndex`（新規）を `crates/alto-scene` に置く。レベル0のタイル座標の格子（256 画素）に、交差するノードの `RIdx` を並べた表。ノードの境界は、変換後のタイル範囲（`RContent::Tiles` はタイル鍵の範囲、グループは子の和、ベクター、テキストはパスの境界）から求める。無限のノード（調整層など）は全タイルに載せる別の表にする。
- `query(tile: TileRect) -> impl Iterator<Item = RIdx>`（新規）。レベルの高いタイルは、含まれるレベル0のタイルの範囲の和を取る。結果は `RIdx` の昇順（木の順序）で返し、合成の順序を変えない。
- 上限：格子は最大 4096 × 4096 タイル（文書の辺長 65536 px の上限と一致）。ノード数の上限は文書の既存の上限に従う。メモリは `RenderScene` と一緒に数え、WASM の計上に含める。
- 更新：`scene_damage` が返す変更の範囲のノードだけを入れ直す。全体の作り直しは `RenderScene` が新しく作られたとき。

### 形式と通信

- 文書の形式、`PROTOCOL`、描画の版：変えない。索引は文書に書かず、保存しない。

### 作業手順

**Files:** Create `crates/alto-scene/src/index.rs`（新規）。Modify `crates/alto-scene/src/lib.rs`、`crates/alto-gpu/src/renderer/jobs.rs`、`crates/alto-gpu/src/renderer/compose.rs`。

- [ ] 現在の `commands_ms` の割合を記録する（`doc/verification/COLD-BREAKDOWN.md`（新規）。PERF-3 と共用）。判定条件で採否を書く。見送りなら、ここで終える。
- [ ] 試験 `index_returns_the_same_nodes_as_the_linear_scan`（proptest。乱数のシーンで、索引の結果が全ノードの線形走査と、順序まで一致）。
- [ ] 試験 `an_edit_updates_the_index_for_changed_bounds`（編集後の索引が、作り直した索引と一致）、`infinite_nodes_are_returned_for_every_tile`。
- [ ] 画素が不変であることを parity 群（`tests/parity/` の既存行）で確かめる。
- [ ] 完了条件：判定に使った行の `commands_ms` が短縮した記録。`sh scripts/check.sh native`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る：PERF-5（計測の足場）。
- 後に続く：PERF-1（計画の費用を下げてから測る）。

## PERF-3 コールド描画 250 ms

### 現状

- 目標 250 ms は未達で、約束ではない。設計書 `doc/design/COLD-RENDERING-DESIGN.md` は「最終的な希望値」と書く。今の値（上の表）は、B1〜B8 の C2、C3 が 276〜297 ms、perf 群の fit コールド表示が `4k-20` 618〜626 ms、`fx` 336〜356 ms、`stress-64mpx` 380〜382 ms、`text-vector` 2187〜2267 ms である。倍率とキャンバスの大きさが違うので、行どうしを比べない。
- 05 の IMG-4 は、判定の指標を `stress-64mpx` の fit コールド表示（377〜380 ms）にした。PERF-3 も同じ行を使う。
- 設計書のC1（新規文書の初回）、C4（復旧）、C5（外部ファイル）の段階別計時は、復旧の内部については未実装（PERF-5）。C2、C3 は実装済み（COLD-RENDERING-RESULTS）。
- 堅牢化 E で、`text-vector` の fit が約 61 秒から約 2.2 秒になった（5件の修正）。ラスターの fixture は修正前後で差がない。
- 派生ディスクキャッシュ（設計書 C）は、初めて開く文書に効かず、読み戻しと OPFS の費用が未測定なので、有効にしていない。

### 11-04 からの変化

- 11-04 は「目標 250 ms は B2 と同じ値」と書いた。B2 は今 2.7〜4.6 ms で、別の測定である。目標の根拠となる行は、判定の指標として上の行に切り替える。
- 11-04 の「C1 の内訳が要る」は変わらないが、perf 群が内訳の行を増やした。
- `text-vector` の fit は約 61 秒から約 2.2 秒になった（E-4、E-7〜E-10）。残りは線分の処理とテクスチャへの書き込み（E-10 のプロファイル）で、これは PERF-1、PERF-2 の対象ではない。

### 設計

**先に測る。** 実装は無い。選択と記録だけである。

1. PERF-5 で段階別計時が入った後、`4k-20`、`fx`、`stress-64mpx`、`text-vector` の fit コールド表示を、`RenderProfile` と CPU プロファイル（CDP）で分ける。段は、計画、入力の取得と復号、GPU への転送、提出、GPU の完了。
2. 律速の段に当たる項目だけを実装する。

| 律速 | 選ぶ項目 |
|---|---|
| 計画（ノード探索） | PERF-2 |
| 入力の共有・提出の回数 | PERF-1 |
| 効果付き文書の1タイルの同期時間 | PERF-4 |
| 重いグループの再合成 | PERF-7 |
| 復号・検証（`.alto` を開く側） | STO-6、RUN-2 |
| ベクターのラスタライズ（`raster_q8`） | 本書の外。VEC 系で扱う |

3. 各段の後に判定の行を測り、250 ms の達成か、達成できない理由を記録する。250 ms は希望値であり、行ごとに妥当かを測定後に見直す（`text-vector` は桁が違う）。
4. p95 は書かない（コールドの行は n < 20）。中央値と最小〜最大、3回の交互測定で比べる。

### 形式と通信

- 文書の形式、`PROTOCOL`、描画の版：変えない（測定と選択のみ）。

### 作業手順

**Files:** Create `doc/verification/COLD-BREAKDOWN.md`（新規）。

- [ ] PERF-5 の後に、判定の4つの fixture の fit コールド表示の内訳を測り、律速の順に PERF-1、PERF-2、PERF-4、PERF-7、STO-6 を選ぶ。WASM、JS ヒープ、GPU の計上値を別の列に書く。
- [ ] 各段の実装の後に判定の行を測り直し、`COLD-BREAKDOWN.md` に追記する。
- [ ] 完了条件：判定の行について、目標を満たしたか、満たせない理由が記録されている。速度の約束は書かない。

### 依存

- 先に要る：PERF-5。
- 後に続く：PERF-1、PERF-2、PERF-4、PERF-7、IMG-4（再測定の前提）。

## PERF-4 効果付き文書のジョブ化と先読み

### 現状

- 再開可能なジョブになっているのは通常のラスターだけである。`Renderer::resumable_scene`（`crates/alto-gpu/src/renderer/jobs.rs`）は `plain_scene`（`crates/alto-scene/src/damage.rs`）が真で、ストローク中でないときに真になる。効果、マスク、クリップ、ベクター、テキストを含む文書では、先読み（`prefetch_keys`）も空を返す。
- 効果付きの1タイルは `Renderer::compose_tiles`（`crates/alto-gpu/src/renderer/tile_renderer/mod.rs`）が、木を再帰して（`Context::node`、`Context::children`、`crates/alto-gpu/src/renderer/tile_renderer/graph.rs`）1つの command encoder で組み立て、提出して終わる。途中で止まれない。
- 中間の対象（`Target`）は、プールのテクスチャ（`Resources::pool`）の番号と、`Arc<()>` の持ち分（`_lease`）で表す。プールの側は `Weak` の `lease` を持ち、持ち分が残っている間は `Resources::expire` が捨てない。捨てるのは持ち分が無く、`POOL_IDLE_FRAMES`（8 フレーム）使われなかったもの。`Resources::retire` は作り直しのときに全部を `Retired` へ渡す（`crates/alto-gpu/src/renderer/tile_renderer/targets.rs`、`mod.rs`）。
- 効果の段は、効果ごとの `stage`（`stages.rs`）で、ぼかし、色調整、レベル補正など。`RenderPlan::compile`（`crates/alto-scene/src/lib.rs`）は `Pass` の列（Draw、BlendIsolated、Effect、Present）を作れるが、`compose_tiles` はこれを使わない。
- GPU 喪失の後は `Renderer` ごと作り直される（`recoverGpu`）。ジョブと持ち分は `Renderer` の中にあり、一緒に捨てられる。表示の世代は `PresentationSnapshot.deviceEpoch`（`crates/alto-gpu/src/renderer/presentation.rs` の `device_epoch`）である。gpu_epoch という名前は無い。
- 測定。`fx` の fit コールド表示は 336〜356 ms、小さなストロークの確定（fit）は 252〜254 ms（E-4）。1タイルの同期時間は測っていない。

### 11-04 からの変化

- compose_m5 は `compose_tiles` になった。
- 11-04 は効果の段 `stack` ごとに区切ると書いたが、`stack` は木を辿る関数の中の効果の配列で、再開の単位にはならない。再帰の途中で止まれる形にする作業が要る。
- 「GPU 喪失の世代 gpu_epoch」は今の名前では `device_epoch` で、持ち分の破棄はレンダラーの作り直しで済む。

### 設計

**先に測る。**

1. `fx`（ぼかし半径 8、色調整、分離グループ、調整層を含む）の、1タイルの `compose_tiles` の同期時間（最大と中央値）を測る。診断の時刻計測（`RenderProfile::max_step_ms` を `compose_tiles` にも適用する）で、2 ms（`JOB_SLICE_MS` と同じ目標）を超えた回数を記録する。
2. 採用する条件：1タイルの同期時間が 2 ms を超えて、B3 や B8 の p95 に影響を与えている、または効果付き文書の低倍率の先読みが無いために fit のコールド表示が支配されている。
3. 見送る条件：超過が稀で、入力への応答に影響しない。

**採用した場合の形。**

- 再帰の `Context::node` と `Context::children` を、明示のタスクの積み（`PlanTask` と同じ流儀の `EffectTask`（新規））に書き換える。1ステップは1ノードの1段（`stage` 1回、または子の合成1回）。ステップの間は、中間の `Target` と持ち分を `TileJob` と同じ形のジョブ（`EffectJob`（新規））が持つ。持ち分が残るので、`expire` はそのテクスチャを捨てない。
- 提出は1ステップごとに別の encoder で行い、キューの順序で依存を守る。最後の解決（`resolve`）が済むまで、表示キャッシュへ登録しない。
- 取り消し。編集による無効化、視点の変更で不要になったジョブは持ち分を手放す。GPU 喪失では `Renderer` ごと捨てるので、ジョブも消える。喪失を検知したステップは、`is_lost()` で何もせず戻る（`prefetch_step` と同じ）。
- 一致の証明。機能ごとに直接合成（`compose_tiles` の同期経路）と全バイトを比べ、一致した機能から移す（設計書 `COLD-RENDERING-DESIGN.md` の P4）。移していない機能を含む文書は、従来の同期経路に戻す。
- 上限。一時ターゲットの持ち分の合計に上限を置き（GPU 予算の 1/8 を目安、要測定）、超えたら従来の経路に戻す。
- 先読みへの適用は、ジョブ化の後に別の段階で行う（`resumable_scene` の条件を広げる）。

### 形式と通信

- 文書の形式、`PROTOCOL`、描画の版：変えない。画素が変わらないことが条件。

### 作業手順

**Files:** Modify `crates/alto-gpu/src/renderer/tile_renderer/mod.rs`、`crates/alto-gpu/src/renderer/tile_renderer/graph.rs`、`crates/alto-gpu/src/renderer/tile_renderer/stages.rs`、`crates/alto-gpu/src/renderer/jobs.rs`、`crates/alto-gpu/src/renderer/frame.rs`、`crates/alto-scene/src/damage.rs`（`resumable_scene` の条件）。

- [ ] 最初に `fx` の1タイルの同期時間を測り、2 ms の超過を記録する（`doc/verification/COLD-BREAKDOWN.md`（新規））。判定条件で採否を書く。
- [ ] 試験 `a resumed effect job matches the direct composite`（効果の種類ごとに全バイト一致）、`GPU loss during an effect job drops its leases`、`a cancelled effect job releases its targets`（`Resources::expire` の後にプールが縮む）、`visible tiles are still composed before effect prefetch`。
- [ ] 実装、PR。機能ごとに分ける。
- [ ] 完了条件：`fx` で判定の行（fit コールド表示、B3 と B8 の p95）が悪化しない。parity 群が全て一致する。GPU の計上値が 512 MiB の判定内。`sh scripts/check.sh native`、`node scripts/verify-browser.mjs --functional`（Metal と SwiftShader）。

### 依存

- 先に要る：PERF-5。PERF-1 の判定（ジョブの形を共有する）。
- 後に続く：PERF-7（測定を判断材料にする）。

## PERF-5 復旧内部の段階別計時

### 現状

- 復旧は `packages/alto-sdk/src/worker.ts` の `case "recover"` が行う。順に、`loadCandidates`、`loadCandidate`（Storage Worker 経由）、`openRecoveryEntries` または `openRecoveryPackage`（WASM、`crates/alto-wasm/src/versions.rs`、`crates/alto-wasm/src/lib.rs`）、`replayRecords`、`activate`。その後に初回の描画がある。
- WASM の `open_recovery_*` の中は、検証、スナップショットの復元、タイルの登録が1つの呼び出しにまとまっている（`crates/alto-engine/src/versions.rs`、`crates/alto-store/src/lib.rs` の `decode_recovery_entries`）。`wasm32-unknown-unknown` では `std::time::Instant` が使えないので、Rust 側で時刻を取るには、外から時計を渡す必要がある。レンダラーは `Renderer::set_clock` で同じことをしている。
- 時刻の記録は、perf 群の `recover` 行が「Worker の再起動から初回描画まで」を1つの値で持つだけである（`tests/e2e/perf-tests.js`）。`browser-diagnostics.json` を書くのは `scripts/verify-browser.mjs` で、CDP のイベントとシステム情報が入り、復旧の段階は入っていない。
- `engine.status`（`packages/alto-sdk/src/engine.ts`）は `state` と `error` だけを持つ公開の型で、診断の数値を足す場所ではない。診断は `@alto/engine/unstable`（`packages/alto-sdk/src/unstable.ts`）に置く。
- GPU の時間は取っていない。`wgpu` のデバイス作成で timestamp-query の機能を要求していない（`Renderer::with_adapter`）。
- 測定の値。復旧は `4k-20` で約 1.27 秒、B7 は約 1.37 秒（E-5 の4）。段階の内訳は無い。

### 11-04 からの変化

- 復旧の経路が変わった。D-3 で候補の選択が整い（`loadCandidates`）、D-7 で Worker 停止と GPU 喪失の試験が入った。復旧の呼び出しの並びは 11-04 より多い。
- 11-04 は結果を `engine.status` に返すとした。公開の型を増やさず、`unstable` に置く。
- 時刻の取り方が決まっていなかった。Rust 側の時計の渡し方を決める必要がある。

### 設計

**ステージ（順序つき）。**

| 名前 | 区間 | 取る場所 |
|---|---|---|
| `candidates` | 候補の一覧を読む | `worker.ts`（`performance.now()`） |
| `load` | 選んだ候補の中身を読む | `worker.ts` |
| `verify` | 外側の検査（zip、索引、ハッシュ） | Rust（時計を渡す） |
| `restore` | スナップショットの復元 | Rust |
| `register` | タイルの登録 | Rust |
| `replay` | 追加レコードの再生 | `worker.ts` |
| `activate` | 世代の採用 | `worker.ts` |
| `firstFrame`（新規） | 描画の開始から GPU の完了まで | `worker.ts` と tick |

- Rust 側の3つは、`open_recovery_package`、`open_recovery_entries` の引数に時計（`&dyn Fn() -> f64`）を渡し、`RecoveryStages`（新規）を返す。時計を渡さない呼び出しは時刻を取らない（本番の既存の経路を変えない）。
- GPU の時間。アダプターが timestamp-query を持つときだけ機能を要求し、親タイルごとの GPU 完了時間を、診断として別の欄に出す。持たない環境では欄を出さない。値は画素の計算に使わない（共通規則6）。ブラウザは時刻を丸めることがあるので、値は目安であり、判定に使う場合は桁数を記録する。
- 返し方。`unstable(engine).recoveryStages()`（新規）が、最後の復旧の段階の数値（ms）の表を返す。復旧がなければ `null`。診断を有効にしないときの費用は、時刻の読み取り数回だけで、復旧は1回きりの処理なので、フレーム時間は増えない。
- `browser-diagnostics.json` への記録。perf 群の `recover` 行の追加情報（`stages`）として残し、`scripts/verify-browser.mjs` が `diagnostics.recovery`（新規の欄）へ書く。

### 形式と通信

- 文書の形式、描画の版：変えない。
- `PROTOCOL`：`recoveryStages`（新規）の要求を足すので上げる（要割り当て）。
- 公開 API：`@alto/engine/unstable` に1関数。互換の約束の外（`doc/embedding.md` 第1章）。

### 作業手順

**Files:** Modify `crates/alto-engine/src/versions.rs`（時計を受け取る版）、`crates/alto-wasm/src/versions.rs`、`crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/src/protocol.ts`、`packages/alto-sdk/src/unstable.ts`、`tests/e2e/perf-tests.js`、`scripts/verify-browser.mjs`、`packages/alto-sdk/api/`（更新）。

- [ ] 最初に `4k-20` と `fx` の `recover` 行を3回測り、段階に分ける前の値を記録する。
- [ ] ブラウザ試験 `recovery reports its stages in order`（段階が表の順に並び、各値が0以上で、合計が `recover` の全体の時間を超えない）。Rust の試験 `recovery_stages_are_zero_without_a_clock`（時計を渡さなければ時刻を取らない）。
- [ ] 実装、PR。timestamp-query は別の PR（アダプターが持たない環境でも、既存の機能試験が通ること）。
- [ ] 完了条件：`browser-diagnostics.json` に段階が残る。計測を足す前後で B7 の中央値の範囲が重なる。`sh scripts/check.sh native`、`pnpm --dir packages/alto-sdk build`（`api:check` を含む）、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る：なし。
- 後に続く：PERF-3、PERF-1、PERF-2、PERF-4、PERF-7（内訳が判定の材料）。

## PERF-7 重いグループの中間キャッシュ

### 現状

- 中間キャッシュは無い。効果付きのグループは、タイルごとに `compose_tiles` が毎回再合成する。
- 持ち回れる鍵が今は無い。`RNode::version` は、文書全体の `revision` が全ノードに入る値で（`Engine::render_scene`、`crates/alto-engine/src/lib.rs`）、ノードごとに変わらない。11-04 の「ノードの `version` と子の `version` の合成」は、そのままでは使えない。
- 文書全体の内容の指紋は `crates/alto-doc/src/digest.rs` にあるが、ノードごとではない。
- 一時ターゲットの確保は `Resources::pool`（`crates/alto-gpu/src/renderer/tile_renderer/mod.rs`）で、`Governor` の計上に入る。余裕は約 18 MiB（B6）。
- 測定。効果付きのズームの内訳は無い。`fx` の fit コールド表示は 336〜356 ms、不透明度の変更の確定（fit）は 255〜258 ms（E-4）。

### 11-04 からの変化

- 11-04 の鍵は成り立たない（上）。ノードごとの内容の指紋が要る。
- 一時ターゲットの予算は、堅牢化 E の `Governor` の計上（E-10）に従う。足りなければ捨てる動きは、予算の判定を満たす必要がある。

### 設計

**先に測る。** PERF-4 の測定を材料にする。

1. 効果付きのズーム（`fx`、低倍率への切り替え）で、グループの再合成が占める時間を、グループごとに記録する。
2. 採用する条件：変化の少ないグループの再合成が、判定の行（`fx` の fit コールド表示、ズーム後の再描画）の半分以上を占め、同じグループが同じ内容で何度も合成されている（ヒット率の見込みが高い）。
3. 見送る条件：上を満たさない。または PERF-4 のジョブ化で応答が足りる。

**採用した場合の形。**

- ノードごとの内容の指紋（`SubtreeDigest`（新規））を `RenderScene` に足す。指紋は、ノードの変換、不透明度、合成方法、効果、マスクと、子の指紋、タイルのハッシュから計算する。`Engine::render_scene` で計算し、文書の `StateDigest`（`crates/alto-doc/src/digest.rs`）と同じく、編集の影響の範囲だけを更新する。
- 鍵は `(指紋, レベル, タイル座標)`。値は一時ターゲットのテクスチャ。確保は `Governor` の予算から行い、上限を GPU 予算の 1/16 に置く。足りなければ最近使っていないものから捨てる（`expire` と同じ流儀）。
- キャッシュの有無で結果のバイトが変わらない（共通規則1、6）。
- 編集で子が変わると、指紋が変わり、古い項目は使われなくなって捨てられる。

### 形式と通信

- 文書の形式、描画の版：変えない。指紋は文書に書かない。
- `PROTOCOL`：変えない。
- 公開 API：変えない。

### 作業手順

**Files:** Modify `crates/alto-scene/src/lib.rs`（指紋の欄）、`crates/alto-engine/src/lib.rs`（`render_scene`）、`crates/alto-gpu/src/renderer/tile_renderer/mod.rs`、`crates/alto-gpu/src/renderer/tile_renderer/graph.rs`。

- [ ] 測定の結果から採否を `doc/verification/COLD-BREAKDOWN.md`（新規）に書く。見送りなら、ここで終える。
- [ ] 試験 `a cached group invalidates when a child changes`、`cache eviction under budget redraws the same bytes`、`the cache never exceeds its share of the GPU budget`、`a subtree digest changes only for the edited branch`。
- [ ] 実装、PR。
- [ ] 完了条件：判定の行が短縮した記録。parity 群が全て一致。GPU の計上値が 512 MiB の判定内。`sh scripts/check.sh native`、`node scripts/verify-browser.mjs --functional`。

### 依存

- 先に要る：PERF-4（測定）、PERF-5。
- 後に続く：なし。

## PERF-6 低速環境でのアイドル限定切替

### 現状

- 先読みは Worker の `schedulePrefetch`（`packages/alto-sdk/src/worker.ts`）が1ステップずつ呼ぶ。ステップの間にイベントループへ戻るので、入力は先に処理される。
- 既に簡単な制限がある。フレームが落ち着くたびに、先読みの累計時間（`prefetchSpentMs`）を0に戻す。累計が 4 ms 以上になると、最後のメッセージから 100 ms 経つまで先読みを待つ。ステップ1回ごとの長さは見ていない。
- 1ステップの目標は約 1.8 ms（Rust の `JOB_SLICE_MS`）。1ステップの長さは `PrefetchStatus::max_step_ms` に最大値だけが残る。
- 低速環境（SwiftShader、内蔵 Chromium など）で先読みの1ステップが長くなる実測はまだ無い。

### 11-04 からの変化

- 累計 4 ms の簡易な制限は 11-04 の時点から同じ形である。ヒステリシスつきの切替は未実装。
- 11-04 の提案（2 ms を8回超えたらアイドル限定、64回で戻す）は残す。ただし既存の累計の制限との関係を決める必要がある。

### 設計

**先に測る。** SwiftShader と Metal で、先読みの1ステップの長さの分布（`worker.ts` が `prefetchStep` の前後で測る）を記録する。2 ms を超える割合が低い環境では、本項目は要らない。

**採用した場合の形。**

- `PrefetchPace`（新規）。置き場は `packages/alto-sdk/src/prefetch-pace.ts`（新規）。純粋な状態機械。入力は1ステップの長さ（ms）と時刻。状態は「通常」と「アイドル限定」。
- 通常から切り替える条件：1ステップが 2 ms を超える回が連続8回。
- 戻る条件：2 ms 未満が連続64回。途中で1回でも超えたら数え直す。
- アイドル限定の間は、最後の入力から 200 ms 以上経ってから先読みを1ステップ動かす。
- 可視の描画（`wantTick`）には影響させない。先読みだけを止める。
- 既存の累計 4 ms の制限は、この状態機械に置き換える（二重にしない）。数値（2 ms、8回、64回、200 ms）は既定で、測定で見直す。利用者の決定は要らない。

### 形式と通信

- 文書の形式、描画の版：変えない。
- `PROTOCOL`：変えない。Worker の内部の振る舞いで、メッセージを増やさない。
- 公開 API：変えない。

### 作業手順

**Files:** Create `packages/alto-sdk/src/prefetch-pace.ts`（新規）、`packages/alto-sdk/test/prefetch-pace.test.js`（新規）。Modify `packages/alto-sdk/src/worker.ts`、`packages/alto-sdk/build.mjs`（必要なら）。

- [ ] 1ステップの長さの分布を、SwiftShader と Metal で測り、判定する。
- [ ] 試験 `slow_steps_switch_to_idle_only`、`hysteresis_needs_many_fast_steps_to_switch_back`、`visible_frames_ignore_the_switch`、`one_slow_step_resets_the_fast_count`。純関数なので、Node の試験で動かす（`packages/alto-sdk/test/present-budget.test.js` と同じ形）。
- [ ] 実装、PR。
- [ ] 完了条件：`pnpm --dir packages/alto-sdk build`、`node --test packages/alto-sdk/test/*.test.js`、`node scripts/verify-browser.mjs --functional`。B1 と B3 の p95 が悪化しない。

### 依存

- 先に要る：なし。
- 後に続く：PERF-4（先読みを効果付き文書へ広げるとき、同じ切替を使う）。

## Review Focus

- 先読みとジョブ化で、可視タイルの優先が崩れないこと（中心に近い順、可視を先読みで退避させない）。
- 中断したジョブの一時ターゲットの解放（持ち分の手放し、`Resources::expire`、`Resources::retire`）。
- 計測のコードが本番のフレーム時間を増やさないこと。診断を有効にしたときだけ時刻を取る。
- ビルドが2種類になる場合に、SDK の `prepack`、`wasm/SHA256SUMS`、`verification/build.json` の対象がどう変わるか。
- GPU 喪失のとき、新しいジョブが古いデバイスの資源を使わないこと。`Renderer` ごと作り直されるので、ジョブがレンダラーの外に資源を持たない（`PresentationSnapshot.deviceEpoch` で確かめる）。
- WASM メモリ、JS ヒープ、GPU の計上値を混ぜて書かないこと。GPU の計上値を VRAM と書かない。
- 速度の約束を書かないこと。p95 は n が 20 以上のときだけ。
