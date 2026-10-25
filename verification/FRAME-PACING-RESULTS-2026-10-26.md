# 表示を GPU の 1 フレーム先までに抑える（2026-10-26）

背景：[GPU-LOSS-RESIZE-TRIGGER-2026-10-25.md](GPU-LOSS-RESIZE-TRIGGER-2026-10-25.md)。調査記録は `claude/chrome-bug-repro` ブランチの `verification/CHROME-BUG-557281760-2026-10-25.md`。

## 変更

Chrome では、Worker のキャンバスで GPU の処理が遅れているときに大きさを変えると、デバイスを失うことがある（crbug.com/557281760）。報告の対照実験では、処理中のフレームを 1 枚までに抑えると失わなかった。Alto も起動中は `tick()` を続けて呼び、GPU の完了を待たずに表示していた。

- `Renderer::render`：前の表示の GPU の処理が終わるまで、次の表示（`getCurrentTexture` と present）をしない。
  - タイルの合成はそのまま進める。
  - 表示しなかったフレームは `FrameReport.deferred` / フレームの情報の `presentDeferred` で知らせる。
  - GPU が追いついた後の tick で、最新の状態を表示する。
- Worker：
  - 表示を保留し、しかも新しく合成したタイルがないときは、`runtime.submittedWorkDone()` を待つ。待つのは最大 50 ms で、SwiftShader で CPU を空回りさせないため。
  - `waitFrame` は、保留中のフレームがなくなるまで待つ。
- `submittedWorkDone()`（新規）：`Runtime` を借りたままにしない Promise を返す。
  - 最初は `gpuIdle()` を使い、50 ms の上限と競わせていた。上限が先に来ると、`gpuIdle()` の非同期メソッドが `Runtime` を借りたまま残り、次の `tick()` が「recursive use of an object」で失敗した。CI の最初の比較で、15 回中 14 回の機能試験の失敗として出た。
  - 上限を 0 ms にして手元で比べると、古い待ち方は同じエラーで止まり、新しい待ち方は 89 件すべてを通った。
- 試験：`a frame held back for the GPU is presented once the GPU catches up`（組み込み）。素早く 40 回パンした後、最後のフレームが保留のままでなく、最新の視点で表示されることを確かめる。

## CI での比較

条件は前回と同じ：macOS、Chrome for Testing 156.0.8078.47、SwiftShader、`gpu_lifecycle=true`、`lifecycle_implementation=sdk`、`surface_recording=true`。

- 基準：`codex/gpu-loss-ci`（b3b70c1）。サイズ変更をまとめる修正（e4a3b5d）を含まない版。
- 比較：同じものに、この表示の調整だけを加えた `claude/gpu-loss-paced`（eb3bd91）。表示の調整のコードは、製品の変更と同一。WASM はバイト単位で一致する。
- 15 回ずつ交互に実行した（40211366913〜40211429374）。

GPU の喪失は、WebGPU の処理が `ProduceSkia: ... non-existent mailbox` で失敗した実行として数えた。この 4 件は、どれもデバイスを失っている（「external Instance」の文言）。

| 条件 | 実行 | 喪失した実行 | うち起動中 | 試験の失敗 |
|---|---|---|---|---|
| 基準 | 15 | 4 | 3 | 4 |
| 表示の調整あり | 15 | 0 | 0 | 0 |

- **偶然の可能性：** 調整に効果がないとして、4 回の喪失がすべて基準側に入る確率は 0.050（片側 Fisher の正確検定）。
- **最初の比較（調整に不具合があった版）も合わせた場合：** 基準 30 回中 8 回、調整あり 30 回中 0 回で、確率は 0.0023。
  - ただし最初の比較の調整ありの版は、機能試験の 3 件目で止まっていた。その後の機能試験（約 29 件）の間は、喪失が起きる機会がなかった。
  - 起動中の喪失だけに絞ると、両方の比較を合わせて基準 30 回中 4 回、調整あり 30 回中 0 回。
- **別の文言について：** `ProduceOverlay: ... non-existent mailbox`（表示の側）は、両方の版に出た（基準 1 回、調整あり 3 回）。
  - どれも、日誌を 2 つ壊してから外部のファイルを開く試験の最中で、Worker を作り直す場面だった。
  - デバイスは失われず、試験も通っている。今回の喪失とは別に扱った。
  - 前回の報告は「`non-existent mailbox` か `SURFACE-FAILURE`」で数えており、このような行も喪失に含めていた可能性がある。

## 手元の確認

- ブラウザの試験 89 件（機能 32・M5〜M8 33・組み込み 24）：SwiftShader と Metal の両方で成功。
- `cargo test` / `clippy`（native と wasm32）、`cargo fmt`、SDK のビルド、`api:check`、SHA256SUMS：成功。
- ベンチマーク（Metal、Chrome 155、調整なし・調整ありを交互に 2 回ずつ）：すべて基準を満たした。
  - B3（入力から表示まで p95）：調整なし 16.8 / 14.4 ms、調整あり 12.8 / 20.7 ms。ばらつきの範囲で、差は判断できない。
  - B1：2.4 / 5.7 → 3.0 / 5.0 ms。そのほかもほぼ同じ。
  - SwiftShader でのベンチマークは、調整の有無にかかわらず「GPU budget too small for one display tile」で失敗する（もとからの制約。CI は実行しない）。

## 分かっていないこと

- Chrome の内部の原因は、変わらず分からない。今回の調整は、報告の対照実験と同じ条件を避けるもので、Chrome を直すものではない。
- サイズ変更をまとめる修正（e4a3b5d）と、どちらがどれだけ効いているかは切り分けていない。PR #3 には両方が入る。
- 確かめたのは macOS、Chrome 156、SwiftShader の組み合わせだけ。
