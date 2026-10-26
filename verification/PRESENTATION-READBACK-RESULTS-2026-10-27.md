# 表示保証・読み戻し予算の実装と検証

2026-10-27 / 基準 main: `1f16c8094bdb847ddd18fa8ffdf7ab24621b80f4`。
[設計書](../docs/design/2026-10-27-presentation-readback-guarantees.md) の変更を実装した。ソース、配布 WASM、SDK dist、API snapshot、内部 protocol 5 を同時に更新している。旧 protocol 4 の Worker / WASM を混ぜない。

## 実装

- 全てのエンジン所有 GPU buffer / texture 作成を共通 Governor に集約。割当て ID、作成前予約、所有移動、退役、queue 完了による回収、操作の高水位を管理する。常駐と退役を二重加算せず、`budgetUsed = gpuBytes + pendingAllocation + reserved <= budgetLimit` とする。固定配列に収まる表示タイルはバイト数へ再加算しない。1 MiB の作業枠と読み戻し枠を先に予約する。
- device ごとに読み戻し Pool を 1 本確保する。容量は `min(8 MiB, budget / 16)` を 256 KiB 単位へ切り下げる。64 MiB では 16 枚、128 / 256 / 512 MiB では 32 枚。排他的な lease によって mapped 中の再利用を防ぐ。初期確保の OOM は 1 枚まで有界に縮小し、validation は別エラーにする。
- 確定はバッチごとに map → コピー → 圧縮済み private staging → 元の GPU タイル解放を行う。全生画素を一つの Vec に集積しない。staging hash を GC root とし、decoded cache への追加を避ける。全件成功後に SetTiles / SetMask を一度だけ適用する。
- map 失敗・キャンセル・30 秒の map timeout では確定前 staging を破棄する。独立した `FinishAbort` は async Runtime の借用中にもキャンセルできる。CPU 確定後の journal 組立て失敗は確定済み patch を通知し、storage-failed にする。
- readTiles / export も Pool を共有し、Worker の export 枚数は実容量へ追従する。確定用の圧縮データとコピー、export の配列に対する保守的な CPU 操作予算は 512 MiB。既存文書やランタイム全体の RAM 制限ではない。
- 初回や文書交換は Preparing。可視領域の詳細が揃うか、同じ文書の全体 overview が完成するまで部分 present を行わない。切替不能なら旧 view を保持する。透明画素は準備済みとして扱い、既存の不透明な present shader により旧画像が透明部分から透けない。
- Active / Building の overview を既存の表示配列内で保持する。Active の slot を保護し、完成時に交換する。DFS 作業枠 `1 + 3L` と両世代の root を予約し、必要なら表示の admitted LOD を粗くする。指定レベルの readback の品質は変更しない。
- 必須の可視／overview 仕事は tick でも進む。任意の idle prefetch が無効でも循環待ちしない。可視タイルを作ったスライスや present 完了待ちでは余分な overview を進めない。
- typed / frozen な `engine.presentation` と `presentation` イベントを追加。view、文書、scene、surface、device の世代を区別し、GPU 完了確認後に interactionReady とする。別文書の旧画面を公開しない。付属 PointerEvent adapter は表示待ちの入力を拒否し、ストローク中の viewport 送信を保留する。同期 viewport API と文書座標の編集 API は維持する。

## 機能検証

SwiftShader / Chrome 155.0.8059.84：機能 36、M5 9、M6 11、M7 9、M8 6、組み込み 37 の **108 件、失敗 0**。[生データ](presentation-readback-swiftshader-2026-10-27.json)。別の Vite ビルドで minimal / React の 2 例も成功。[例の結果](presentation-readback-examples-2026-10-27.json)。

組み込みに次の 6 件を追加した。1 件に複数条件を含む。

1. 64 / 512 MiB で 1 / 16 / 17 / 31 / 32 / 33 枚を確定。canvas 外も含む全正規キー、バッチ数、8 / 4 MiB の固定容量、Undo 一回、Redo の hash、課金上限を確認。
2. 65 枚の第 1 / 中間 / 最終バッチで失敗。digest / revision / Undo / Redo 不変、GPU 資源・map の解放、次のストロークが成功することを確認。
3. async 確定中のキャンセル。部分確定がなく、全 staging と GPU タイルを解放することを確認。
4. CPU 確定直後の journal 生成失敗。CPU / mirror の編集・履歴が一回ぶん一致し、保存停止・緊急書出しへ進めることを確認。
5. 任意 prefetch 無効での完了、Snapshot の freeze、Holding、未公開 view の PointerEvent 拒否、ストローク中の zoom / rotation の保留と座標維持、GPU 喪失後の保留 viewport の解除と version 継承、別文書への fallback 禁止を確認。
6. 64 MiB / 42 display slots、回転を含む大きな view の LOD admission。全被覆の完了と、原寸 readback を変えないことを確認。

Rust では Governor の予約・退役・所有移動・異なる epoch・読み戻し用の枠が残量ゼロでも使える条件、staging の GC と生画素 cache 回避、表示の準備状態、キャンセルと commit の境界を検査する。Rust **136 件**と Node **7 件**は成功。全 workspace、native / WASM Clippy、fresh 配布物の一致、SDK 型・公開 API、Node の保存回復テストを `scripts/check.sh` で検証する。

RGBA16F の Legacy / Pooled / Disjoint / Dense / Batched 比較も 10 シーンで画素一致。[生データ](presentation-readback-vector-2026-10-27.json)。比較用 Vello は製品 WASM へリンクしない。

## Metal の測定

Chrome 155 / Apple Metal、4K・20 レイヤー、970×472 の surface、physical view、DPR 1。CPU/GPU の他の試験を同時実行せず新旧を逐次計測した。新旧それぞれ一回の実行であり、速度改善の統計的証明ではない。

| 指標 | 基準 main | 今回 |
|---|---:|---:|
| 389 枚の全面ストローク確定 | 129.4 ms | 130.7 ms |
| 読み戻し buffer 要求量 | 97.25 MiB（旧コードから計算） | 8 MiB（13 バッチ / 389 枚 / mapped 0） |
| GPU 操作ピーク、予約込み | 旧診断では取得不可 | 493.75 MiB |
| B6 フレームサンプルの管理量 | 485.065 MiB（readback 未計上） | 492.751 MiB（readback 込み） |
| 確定後の GPU 管理量 | 約 193 MiB | 約 201 MiB、予約込み 202 MiB |
| WASM + decode pool の高水位上界 | 222.82 MiB | 220.82 MiB |
| warm frame p95 | 3.4 ms | 3.4 ms |
| 入力から GPU 完了 p95 | 8.2 ms | 8.1 ms |
| GPU cold | 165.7 ms | 198.3 ms |
| CPU + GPU cold | 163.1 ms | 197.2 ms |
| 小編集の確定から再描画、中央値 zoom 1 | 5.2 ms | 11.2 ms |
| 同、全体表示 | 4.3 ms | 12.4 ms |

新実装は既存ベンチ 13 項目の閾値を満たした。初回被覆・GPU 完了の確認、常時の Governor、overview の作業に伴うコールド／小編集の遅延増加も記録する。この変更を全体の高速化とは評価しない。実際の OS scanout、物理 VRAM / driver のピークは計測していない。旧フレーム値 485 MiB と新値 493 MiB を単純比較して物理メモリの増減と解釈しない。

[今回](presentation-readback-metal-2026-10-27.json) / [基準 main に確定時間の計測だけを追加した実行](presentation-readback-baseline-metal-2026-10-27.json)。基準の製品 WASM は main の配布物をそのまま使用した。

## 確認範囲の限界

実 GPU の OOM を意図的に起こした縮小試行、30 秒間 map が応答しない実ドライバ、最小 1 枚でも確保不能、極端な level 12 の多数 root と世代交換の全組合せは、今回の実測では網羅していない。設計の受入表 23 行をすべて独立した故障注入試験で消化したという意味ではない。既存の device-loss 復旧・選択／変形／mask・透明画素・canvas 外タイルの回帰試験も含めて判断する。

厳密に制限するのは **エンジンが WebGPU へ要求する buffer / texture の帳簿上の量**。surface、shader / pipeline、ブラウザ・ドライバの内部確保や RAM / VRAM 全体の物理上限は、この API から保証できない。Chromium の間欠的な GPU 喪失自体の根治や、未確定ストロークのクラッシュ復元も本変更の完了条件には含めない。

## CI と配布物

配布物の bytes / gzip / SHA-256 は [build.json](build.json)、全対象ファイルは [SHA256SUMS](../SHA256SUMS) で照合する。CI の URL と実行結果は、push 後に追記する。
