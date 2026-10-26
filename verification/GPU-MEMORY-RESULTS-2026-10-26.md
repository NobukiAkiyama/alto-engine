# GPU メモリ制御の実装・検証


生の検証JSONはこのディレクトリに置かない。新しい実行は CI の artifact `alto-browser-reports`（GPU lifecycle の手動実行は `gpu-lifecycle-<OS>`）、ローカルは gitignore した `verification/out/`。以下のかつてのファイル名は、この変更より前の git 履歴にある。

2026-10-26。PR #6 の文書を実装へ進め、既存の MRT8 と main の present pacing を統合した。キャンバス外を含む正規ラスタタイルは維持する。

## 実装

- `crates/alto-gpu/src/renderer/memory.rs`：固定・占有・効果・ストローク・ベクターの推定、予算別の配列層数、device ごとの退役台帳。合計は4区分の和で、占有と退役の診断は二重加算しない。完了 callback は台帳だけを回収し、通常の同一 queue 再利用を待たせない。
- `renderer/cache.rs`：可視と完成済み最粗を保持し、遠方、非理想レベル、距離、古さで退避する。内容アトラスも encoder の pin を維持したまま距離を優先する。圧力後の最粗は子タイルから一枚ずつ構築し、未提出のコピーや構築中の子を保護する。
- `renderer/m5.rs` と `effects.wgsl`：64px バケット、`Target` の lease による寿命追跡、同一 encoder の後続 clear と書き込みで再利用する。全物理領域を透明化し、論理 A/B/clip/output を別々に制限する。8フレーム未使用の資源を明示的に退役する。
- WASM の末尾に診断を追加し、SDK Worker が `gpu` グループとして復号する。合計の既存フィールドと既存の位置は維持する。Playground に4区分を表示し、B6 には同一ピークフレームの内訳とストローク終了後を記録する。
- MRT8 のメッシュ・カバレッジ・uniform・旧カバレッジ交換時にも退役分を考慮する。既存の Gaussian 参照テスト本体、Worker の50ms待機上限、README、ライセンスには変更を加えていない。

## 確保量

配列層数の上限が512以上の場合。1MiB = 1048576 bytes。完了待ちの一時コマンドバッファは別途 `retiredFixed` に含み、固定配列予約は `fixed-retiredFixed` で読む。

| 予算 | 内容 / 表示層 | 固定配列 + scratch | 以前 |
|---|---|---|---|
| 64 MiB | 82 / 42 | 32 MiB | 128 MiB へ切り上げ、129 MiB予約 |
| 128 MiB | 168 / 84 | 64 MiB | 129 MiB |
| 256 MiB | 338 / 170 | 128 MiB | 193 MiB |
| 512 MiB | 512 / 256 | 193 MiB | 193 MiB |

64MiB未満は拒否する。配列層数が256までの SwiftShader では512MiB設定時の固定予約は129MiBとなる。これはアダプター上限によるもので、Metalの193MiBと同一と見なさない。64/128/256MiBの各実ブラウザ試験で固定予約、予算内の報告値、ブラシ中心のRGBAを検査した。

## 効果の再利用

大きい白画像＋半径20のぼかしでバケットを汚した後、小さい矩形＋半径8/9/8で使い直した。256×256の全画素をCPUの `intervalBlur` と許容差2で比較し、残留ハローがないことを確認した。続けて合計32段の効果（ぼかし＋31段の恒等色調整）を処理し、RGBAバイト列が完全一致した。

| 環境 | 物理プール確保回数 | 通常 / 32段後の再利用回数 | 通常 / 32段後の効果確保量 |
|---|---|---|---|
| Chrome155 / Metal | 7 | 112 / 148 | 3,047,432 / 3,047,432 bytes |
| Chrome155 / SwiftShader | 7 | 95 / 131 | 3,047,432 / 3,047,432 bytes |

約2.91MiBを維持した。旧実装との速度比較ではなく、今回のプールがノード数だけ増えないことと実際の再利用を検査した結果である。フレームの進み方により回数は変わる。

## 正しさと復旧

- Rust130件、native/wasm clippy、SDK build、公開API検査、Nodeストレージテストも成功。
- Metal/SwiftShaderそれぞれ機能36、M5 9、M6 11、M7 9、M8 6、組み込み31の計102件を実行、失敗0。
- 新規テスト：合計と部分集合、64/128/256MiB描画、64MiB未満拒否、queue順の赤→緑コピーとsubmit後destroy、同一encoderの旧読み→clear→新読み、圧力下の最粗構築とパン／回転後保持、ぼかしの汚れたバケット再利用、32段の恒等効果。
- キャンバス外の正規タイル `(-2,0)` を含むストロークを保持し、レイヤーを416px移動すると `(32,32)=[255,0,0,255]` となる。Undo/Redo、ZIP再読込、GPU喪失からの復旧後も、全正規タイルのキー・ハッシュと画素が維持される。
- 既存の選択付き・変形付きブラシ、シームをまたぐGaussian、クリッピング、効果、MRT8、OPFS障害からの復旧の検査も通った。

生データ：Metal（`gpu-memory-metal-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）、SwiftShader（`gpu-memory-swiftshader-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。Metal機能試験は最終的なモジュール分割／ストローク新規確保の上限検査を追加する直前、SwiftShader機能試験はその追加後に実行。画素演算は同一である。

## Metalの既存ベンチマーク

B1–B8/C2/C3の既存13項目が成功。B2ズーム3.2ms、B8小ストローク中央値5.3ms（zoom1）/5.2ms（全体）。絶対値をCIの合格基準には使わない。

| B6サンプル | fixed | liveFixed（非加算） | effects | stroke | vector | 合計 |
|---|---|---|---|---|---|---|
| 全面ブラシflush直後 | 193.315 MiB | 132.75 MiB | 0 | 291.75 MiB | 0 | 485.065 MiB |
| ストローク終了後 | 193.001 MiB | 132.75 MiB | 0 | 0 | 0 | 193.001 MiB |

ピークの固定配列は193MiB、追加の0.315MiBは完了通知前の一時コマンドバッファ。従来値484.75MiBとの差は、この計上追加である。正規データを捨てる190.5MiB削減は行っていない。生データ（`gpu-memory-benchmark-metal-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。

## 追加の寿命・互換性検査

プールのleaseは `Arc`/`Weak` を使い、native RendererをWorkerへ移動できる `Send` のコンパイル時検査を追加した。上記102件はその互換性修正前の機能試験で、修正後のM5 9件・組み込み31件も失敗0で、別記録（`gpu-memory-lease-swiftshader-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）に保存した。ベンチ値は互換性修正前の測定値である。

従来のベクター予算フォールバック試験は永続資源だけに1 byteを足した予算だったが、参照経路でもmeshごとに48 byteの一時uniformを作る。今回からその実測アップロード量も予約し、64 KiBのarenaページには足りない条件を維持する。

MRT8の予算縮小試験では、退役させたカバレッジがまだ占有する間の新規確保を拒否することを明示的に検査する。そのencoderをsubmitして退役を処理し、検証用の完了待ちの後、同じ予算で再描画する。これは通常の再利用に追加したfenceではなく、新規確保の上限が回復することを見る負荷試験である。

Viteのminimal/react組み込み例も両方成功した。

MRT8の全10シーンはLegacyとRGBA16Fバイト単位で一致し、同一encoderでの寸法変更、待機しないsubmit列、arena予算フォールバック、退役後の再確保も成功した。生データ（`gpu-memory-vector-swiftshader-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。

## 境界

報告値はエンジンが管理する予約／描画資源と退役分の推定であり、ドライバVRAM、Surface、ブラウザ内部、readbackバッファまで含む完全な物理ピークではない。`endBrush` の一括readbackを分割する変更も含めていない。

最粗は可視フレームを描いた後のスライスで作る。完成後は保持するが、最初の圧力発生から完成までの間のズーム表示について、穴ゼロは保証しない。広い可視範囲や必要なpinが表示層数を超えた場合は、タイルを黙って省かず既存のbudgetエラーを返す。

Chromeの間欠的なSurface/GPU喪失の根治を証明する変更ではない。既存のpresent pacingを維持し、復旧の画素と正規データを回帰試験で検査する。
