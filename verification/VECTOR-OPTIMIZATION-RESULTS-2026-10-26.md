# ベクター描画改善の実装・検証結果

2026-10-26 JST。対象は `35be75a` と既存の SDK 修正を含む作業ツリー。これはベクター描画の P1 / P2a の検証であり、Canvas の間欠的な GPU 喪失を解決したという報告ではない。

## 実装

- 通常の Paths の描画を既定で Batched にした。lyon、4x MSAA、R8 coverage、premultiplied RGBA16F、shader の座標計算は維持する。
- uniform arena は動的 offset を使い、未提出の各描画に別の領域を渡す。実際の submit 後にだけカーソルを戻す。同じ queue の順序で再利用し、GPU 完了待ちや追加 submit は導入しない。
- arena は 256 slots / page、最大16 pages。device の alignment / buffer limit と既存の GPU 予算を適用する。Apple M5 では64 KiB / page。上限・予算不足では旧経路へ退避する。
- パスの全 Mesh バッチの scissor を覆う保守的な矩形を使い、接触・交差しない連続パスだけをまとめる。最大128パス、4096候補比較、4096区画でグループを閉じる。
- fill / stroke のローカルな順序とグループ間の順序を維持する。重なるパスは singleton とし、pass 数は従来に戻るが uniform の再利用は続ける。
- coverage の texture view と paint bind group を再利用する。text / glyph、ベクターマスクは旧描画経路を維持する。
- Legacy / Pooled / Batched を同じ adapter / device / queue 上で強制比較する任意ビルドの検証器を追加した。安定 SDK の API は変更しない。CI に SwiftShader の厳密な画素比較を追加した。

主なコードは `crates/alto-gpu/src/renderer/vector.rs`、`vector/arena.rs`、`vector/batch.rs`、`m5.rs`。比較用コードは `crates/alto-wasm/src/vector_probe.rs` と `tests/diagnostics/vector-optimization.html`。

## 実 GPU の結果

Chrome 155.0.8059.84、Apple M5 / Metal、512×512、RGBA16F、4x MSAA。独立したブラウザプロファイル5回、それぞれ3フレームの warmup 後100フレームを計測した。方式の実行順を交互にした。表は5回の中央値の中央値。CPU encode から queue 完了通知までの時間であり、GPU timestamp や画面表示完了ではない。readback と validation scope の収集は計時外。

| 入力 | Legacy | Pooled | Batched | Legacy → Batched の pass 数 |
|---|---:|---:|---:|---:|
| 離れた100パス、塗り＋線 | 22.7 ms | 21.8 ms | **1.0 ms** | **401 → 5** |
| 重なる100パス | 22.3 ms | 21.1 ms | 21.3 ms | 401 → 401 |
| 重なる1,000パス | 234.1 ms | 218.5 ms | 221.0 ms | 4001 → 4001 |

離れた100パスは約95.6%短縮。warm の uniform buffer / bind group の作成は200 / 400個から双方0個になった。geometry / paint draw は各200回のままなので、draw 数の削減を達成したとは扱わない。coverage clear / resolve の論理画素数は各52,428,800から524,288へ減った。物理メモリ帯域の実測ではない。

同条件の管理対象 GPU 資源は1,521,920 bytesから1,587,456 bytesへ増加した。追加の arena は65,536 bytes。重なる1,000パスでは arena は524,288 bytes、最大16ページの範囲に収まる。これは管理下の buffer / texture の量であり、ブラウザ全体や driver の総使用量ではない。

生データ、全100サンプル、初回描画、初期化、CPU encode、p95、WASM hash は [Metal の記録](vector-optimization-metal-2026-10-26.json)に保存した。初回値は方式の順序や shader / pipeline の状態に影響されるため、warm の倍率をそのままコールド表示の改善率にはしない。変形など他の5条件は性能採用の100フレーム対象ではなく、画素検査用である。

## 正しさ・配布の確認

- 8条件で Legacy / Pooled / Batched の **RGBA16F 全バイト一致**。離れた形、半透明の重なり、回転・shear・ROI、1,000パス、反射・拡大、stroke-only / cap / join、穴・接触・自己交差、大きな座標を含む。Metal の5プロファイルおよび SwiftShader で差分0。
- 同じ encoder に2回の描画を入れる検査、GPU 完了を待たず連続 submit する検査、arena の予算不足で旧経路に戻す検査を通過した。
- `scripts/check.sh` 成功。Rust 122件、Clippy の native / WASM 検査、SDK build / API check、Node 7件、配布 WASM 2箇所と fresh release build の完全一致、build metadata、SHA256SUMS の照合を含む。任意 `vello-probe` feature も WASM Clippy を通過。
- SwiftShader の既存ブラウザ回帰93件成功：functional32、M5 8、M6 10、M7 9、M8 6、embed28。保存障害・復旧、Undo/Redo、text / mask / effects の既存検査も含む。[回帰記録](vector-regression-swiftshader-2026-10-26.json)、[追加の画素検査](vector-optimization-swiftshader-2026-10-26.json)。
- Apple Metal でも同じ93件を成功した。[Metal の回帰記録](vector-regression-metal-2026-10-26.json)。
- Vite でビルドした `minimal-embed` / `react-embed` の2例も SwiftShader 上で成功。描画、Undo/Redo、PNG export、保存・再読込を実行した。
- 編集全体の既存 C2 / C3・B1〜B8 は変更前後とも13条件を通過した。[前後の記録](vector-editor-regression-metal-2026-10-26.json)。これは各1プロファイルの回帰確認であり、統計的な速度向上の根拠にはしない。ラスター主体の経路なので、今回のベクター改善によるコールド表示高速化を示すものでもない。
- 配布用 WASM を2箇所とも再生成した。4,580,315 bytes、SHA-256 `3455925b0e967e94d286d193b2dc47b6c1252110befa0fffff2128c0d20e2dec`。比較用 WASM は6,310,241 bytes、SHA-256 `5f15ea38425114fe22a2642aa41191115bb2cb79cfdef8ad2c6fca3053bf9881`。vello は通常配布ビルドに追加していない。

## 再現

```sh
sh scripts/build-m0.sh
pnpm --dir packages/alto-sdk build
sh scripts/build-vector-comparison.sh
ALTO_VECTOR_FRAMES=100 ALTO_BROWSER_REPORT_DIR=/tmp/alto-vector-metal \
  node scripts/verify-browser.mjs --vector-optimization
ALTO_BROWSER_GPU=swiftshader ALTO_VECTOR_FRAMES=1 \
  ALTO_BROWSER_REPORT_DIR=/tmp/alto-vector-pixels \
  node scripts/verify-browser.mjs --vector-optimization
ALTO_BROWSER_GPU=swiftshader ALTO_BROWSER_REPORT_DIR=/tmp/alto-regression \
  node scripts/verify-browser.mjs --functional
sh scripts/check.sh
```

採用判定では最初の性能コマンドを独立プロファイルで5回実行し、`ALTO_VECTOR_REVERSE=0 / 1` を交互に設定する。SwiftShader のフレーム時間は性能の採用判断には使わない。

## 今回の範囲

P2a が離れた100パスの目標を満たしたため、paint instancing と vello の本体統合は採用しない。高密度に重なる文書では今回のバッチ化による大きな高速化は期待できず、1,000パスの221 msも対話描画に十分な速度ではない。

全ての実文書、全倍率・極端な数値の組合せ、Safari / Firefox / 他 GPU、vello と同品質での優劣、利用者の制作効率は未検証。Canvas / compositor の GPU 喪失は従来の独立調査に残る。WASM 内部の render 時間短縮を、アプリ全体やコールド起動全体の約23倍高速化として説明しない。
