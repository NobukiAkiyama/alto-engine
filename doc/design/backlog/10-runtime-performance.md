# 実行基盤と性能

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。

**Goal:** 残る性能項目と実行環境の拡張を、測定で採否を決められる形にする。

**Architecture:** Main、Engine Worker、Storage Worker、Decode Worker 群の構成。SharedArrayBuffer なしで全機能が動く。性能は CI の合否条件ではなく、測定はこの Mac の記録である。

## 注意

ここの現状の一部は10月時点の記録で、その後の変更で解消した可能性がある。各タスクの最初の手順で現在の値を測り、解消済みならタスクを閉じる。例：当時未実装だった「中断可能な先読み」（P6）はその後実装され、本書の対象から外した。

## RUN-1 WASM スレッド、SharedArrayBuffer

**設計。** `crossOriginIsolated` のときだけ使う。SAB なしの動作を維持し、差が出るのは速度だけにする。

- WASM を2つビルドする（atomics あり、なし）。atomics 版は nightly と build-std を要するので、現在の状況を最初に再確認する（`rust-toolchain.toml` は 1.99.1 に固定）。
- 用途：入力のリングバッファ、LZ4 と BLAKE3 の並列化。既に Decode Worker があるので、SAB なしでも足りるかを最初に測る。
- 判定条件：B5（保存・読込）と B7（起動）が 20% 以上短縮しないなら採用しない。WASM が2倍になる配布コストと比べる。

## RUN-2 Compute Worker

**設計。** `alto-decode` の Decode Worker と同じ形で、CPU フィルタと符号化を並列にする。現状の Decode Worker プールを「Compute プール」へ広げ、ジョブの種類を増やす。候補は `image.rs` の resize と、保存時の LZ4 とハッシュ。決定性を守るため、結果はタイル単位でハッシュされ、順序に依存しない形にする。測定で `image` の内訳が符号化中心なら採用する。

## RUN-3 ネイティブ

**設計。** `alto-engine` と `alto-gpu` はネイティブでビルドできる（CI の `native` ジョブ）。`examples/native-viewer` は winit と wgpu で `.alto` を開き、パンとズームを行う最小の例。`alto-engine` の公開 API に `render_frame` の同期版を追加する必要があるかを、例を書きながら確かめる。公開 API は `alto-engine` の `native` モジュールに限って SemVer の対象にする（`Engine`、`DocHandle`、`render_frame`、`save`、`open`）。crates.io への公開は最初のリリースの手順に含める。

## RUN-4 readback プール2本化

**設計。** `2026-10-27-presentation-readback-guarantees.md` の第11節のとおり、copy と map を並行化する。本数を先に予約し、同じ故障試験を追加する。B3 の連続描画で読み戻しの待ちが観測された場合に行う。先に計測を足す。

## PERF-1 可視タイルのバッチ化

**設計。** [PERFORMANCE-IMPLEMENTATION-PLAN.md](../PERFORMANCE-IMPLEMENTATION-PLAN.md) の P5。不足している表示タイルをまとめ、入力タイルの取得と転送を共有する。効果付き文書（`compose_m5`）と1タイルで容量を超える場合は従来経路。v3 で全バイト一致を維持する。

## PERF-2 シーン索引

**設計。** タイル座標から交差するノードを引く索引。`COLD-RENDERING-DESIGN.md` の P1。ノード数に対して線形の判定を、グリッドの索引で O(該当数) にする。変更範囲の差分で更新する。

## PERF-3 コールド描画 250 ms

**設計。** 未達の目標。`COLD-RENDERING-RESULTS` の C1 の内訳が必要で、C1、C4、C5 の時刻計測（PERF-5）を先に入れる。PERF-1、PERF-2、PERF-4、STO-6 のどれが効くかを内訳で選ぶ。目標の 250 ms は B2 と同じ値で、初回の冷えた文書では妥当か、測定後に見直す。

## PERF-4 効果付き文書のジョブ化と先読み

**設計。** 通常ラスタだけが中断可能なジョブになっている。`compose_m5` の1タイルを、効果の段（`stack`）ごとに区切って再開可能にする。一時ターゲットの lease をジョブが保持する。[COLD-RENDERING-DESIGN.md](../COLD-RENDERING-DESIGN.md) の P4 のとおり、機能ごとに直接合成との一致を証明してから移す。

## PERF-5 復旧内部の段階別計時

**設計。** `crates/alto-wasm` の復旧経路に、検証、スナップショット復元、タイル登録、初回描画の時刻を入れ、`engine.status` の診断として返す。`browser-diagnostics.json` に記録する。`timestamp-query` が使える環境では GPU 時間も記録し、親タイルごとの GPU 完了時間もここへ含める。

## PERF-7 重いグループの中間キャッシュ

**設計。** 設計書 3.5 の案。頻繁に変わらず再合成の重いグループの結果を、ノードの `version` と子の `version` の合成を鍵にして保持する。一時ターゲットの予算から確保し、足りなければ捨てる。測定で段の時間が支配的なときだけ採用する。効果付きのズームが遅い場合の候補で、PERF-4 の測定を判断材料にする。

## RUN-5 他 queue、外部 GPU API との所有権移譲

**設計。** `2026-10-27-presentation-readback-guarantees.md` の第11節の項目。現在の描画器は1つの queue に閉じている。SDK に `doc.exportTexture(region): Promise<GPUTexture>` と `engine.importTexture(texture, region)` を置く。WebGPU の `GPUDevice` は Worker をまたいで共有できないので、Web では `ImageBitmap` と `OffscreenCanvas` の転送で渡し、同じ device のテクスチャ共有はネイティブだけで提供する。同期の契約（読み取りの完了と所有の移動）を `doc/embedding.md` に先に定める。

ブラウザやドライバー側の間欠的な GPU 喪失そのものは本エンジンで根治できない。復旧と記録（`scripts/gpu-process-crash.mjs`）を維持する。

## PERF-6 低速環境でのアイドル限定切替

**設計。** 先読みの1ステップが 2 ms を超える回数が続いたら（連続 8 回）、先読みをアイドル時（入力なし 200 ms 以上）に限る。ヒステリシスを持たせ、戻るには 2 ms 未満が連続 64 回。可視の描画には影響させない。

## 実装タスク

### Task PERF-5: 段階別計時

**Files:** Modify `crates/alto-wasm/src/lib.rs`、`packages/alto-sdk/src/worker.ts`、`scripts/verify-browser.mjs`。

- [ ] ブラウザ試験 `recovery reports its stages in order`。
- [ ] 実装、PR。

### Task PERF-2 / PERF-1

**Files:** Modify `crates/alto-gpu/src/renderer.rs`、`crates/alto-scene/src/lib.rs`。

**Interfaces:** Produces `SceneIndex::query(tile: TileRect) -> impl Iterator<Item = RIdx>`。

- [ ] 最初に現在の B1、B2、C1 を3回測り、記録する。
- [ ] 試験 `index_returns_the_same_nodes_as_the_linear_scan`（proptest）。
- [ ] 試験 `an_edit_updates_the_index_for_changed_bounds`。
- [ ] parity が不変であることを確かめ、PR。
- [ ] PERF-1：バッチ境界をまたぐ半透明20層、アトラス退避、効果、キャンバス端、GPU 喪失の画素比較。B2 の短縮を記録。短縮がなければマージしない。

### Task PERF-4

- [ ] 最初に効果付き文書の1タイルの同期時間を測り、2 ms の超過を記録する。
- [ ] 試験 `a resumed effect job matches the direct composite`（モードごとに全バイト）。
- [ ] 試験 `GPU loss during an effect job drops its leases`。
- [ ] 実装、PR。

### Task PERF-6

- [ ] 試験 `slow_steps_switch_to_idle_only`、`hysteresis_needs_many_fast_steps_to_switch_back`、`visible_frames_ignore_the_switch`。
- [ ] 実装、PR。

### Task RUN-1 / RUN-2 / RUN-4 の判定

**Files:** Create `doc/verification/RUNTIME-PARALLEL-MEASURE.md`。

- [ ] `image.rs` のベンチで resize、保存の内訳を native と WASM で測る（現状の数値は native のみ）。
- [ ] 判定条件に照らして採否を記録する。

### Task RUN-3: 確認用の例

**Files:** Create `examples/native-viewer/`。

- [ ] `verification/sample-m78.alto` を開いて1フレーム描画し、画素のハッシュが CI の native parity と一致する試験 `native_viewer_renders_the_sample`。
- [ ] 実装、PR。

### Task RUN-5: 外部とのやり取り

**Files:** Modify `doc/embedding.md`、`crates/alto-gpu/src/renderer/readback.rs`、`packages/alto-sdk/src/document.ts`。

- [ ] 同期の契約を文書に書いてから実装する。
- [ ] ブラウザ試験 `an exported bitmap holds the composite after later edits`、`an import after GPU loss is refused`。
- [ ] ネイティブ試験 `a shared texture read waits for the queue`。
- [ ] 実装、PR。

### Task PERF-3 / PERF-7: 律速の解消

**Files:** Create `doc/verification/COLD-BREAKDOWN.md`。

- [ ] PERF-5 の後に C1 と効果付きズームの内訳を測り、律速の順に PERF-1、PERF-2、PERF-4、PERF-7、STO-6 を選ぶ。
- [ ] PERF-7 を選んだ場合：試験 `a cached group invalidates when a child changes`、`cache eviction under budget redraws the same bytes`。
- [ ] 各段の後に C1 を測り、250 ms の達成か、達成できない理由を記録する。

## Review Focus

- 先読みとジョブ化で、可視タイルの優先が崩れないこと。
- 中断したジョブの一時ターゲットの解放（`expire`、`retire`）。
- 計測のコードが本番のフレーム時間を増やさないこと。
- ビルドが2種類になる場合に、SDK の `prepack` と SHA-256 の対象がどう変わるか。
- GPU 喪失の世代（`gpu_epoch`）を新しいジョブが必ず確かめること。
