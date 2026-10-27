# 密集ベクターの MRT8 実装結果


生の検証JSONはこのディレクトリに置かない。新しい実行は CI の artifact `alto-browser-reports`（GPU lifecycle の手動実行は `gpu-lifecycle-<OS>`）、ローカルは gitignore した `verification/out/`。以下のかつてのファイル名は、この変更より前の git 履歴にある。

2026-10-26 JST。対象は `16e4ce0` と、先行 SDK 修正を保持した密集ベクターの作業ツリー。[設計](../design/2026-10-26-dense-vector-mrt.md)。Canvas / compositor の間欠的 GPU 喪失を修正したという報告ではない。

## 変更

通常の Paths に8枚の独立した R8 coverage を使う経路を追加した。各 geometry pipeline は一つの attachment にだけ書き、paint は元のバッチ順で RGBA16F へ合成する。lyon、座標計算、4x MSAA、stroke の coverage union、色の精度は維持する。

8パス以上で disjoint グループ数がパス数の80%以上なら候補にする。離れたパスは従来の disjoint batching のまま。GPU limits / 予算不足では既存の経路を使う。text / glyph / vector mask の適用範囲は拡張していない。M5 の提出境界、Canvas / resize、SDK の安定 API も変更していない。

追加の texture、64-byte dense uniform arena、通常の arena を管理下の予算に含める。使用中の offset を submit 前に上書きしない。ROI 変更時は bind group を作り直し、古い GPU 資源は提出後に解放する。以前の dense scratch が通常描画の予算を圧迫する場合には cache を退役させる。

主なコード：`renderer/vector/dense.rs`、`vector/arena.rs`、`vector.rs`、`vector-dense-coverage.wgsl`、`vector-dense-paint.wgsl`。texture の座標を詰め直す atlas や vello の本体統合は採用していない。

## 実 GPU の単体比較

Chrome 155.0.8059.84 / Apple M5 / Metal、512²、R8 / 4x MSAA → premultiplied RGBA16F。同じ adapter / device / queue、各条件3 warmup＋100フレーム。独立6プロファイルで、実行順の正逆を3回ずつに揃えた。最初の5回も含め全サンプルを保持した。時刻は CPU encode 開始から queue 完了通知までで、GPU timestamp や画面の表示完了ではない。

| 条件 | 先行 Disjoint | 新 Batched（自動選択） | pass 数 |
|---|---:|---:|---:|
| 離れた100パス | 0.90 ms | 0.95 ms | 5 → 5 |
| 重なる100パス | 21.25 ms | 3.45 ms | 401 → 51 |
| 重なる1,000パス | 217.55 ms | **32.25 ms** | **4001 → 501** |

各プロファイル中央値の中央値。密集1,000パスは約85.2%短縮、約6.75倍。疎な条件の差は0.05 msで、個々の観測は0.1 ms程度に量子化されている。CPU encode の中央値も同条件で7.5 msから4.75 msへ短縮した。Legacy / Pooled / Disjoint / Dense 強制 / Batched 自動選択を比較した。全生データ（`dense-vector-metal-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。

1,000パス条件の管理対象 GPU 資源は1,847,068 bytesから11,022,108 bytesへ増えた。差分9,175,040 bytesが追加7組の coverage。warm の uniform buffer / bind group 作成は双方0個。arena は524,288 bytes。driver の内部資源や退役待ちの全一時確保を含む総 GPU 使用量ではない。clear / resolve の論理画素数と draw 数は削減しておらず、主に pass 境界を減らした。

密集条件は5種類の形状を反復した、メッシュが準備済みの入力。1,000種類の複雑な形状の初回 tessellation や、アプリ全体の起動性能を表す値ではない。4枚方式の試作でも画素一致と改善を確認し、8枚方式を正式測定した。試作の数フレームの数値は採用値に混ぜていない。

## 配布 SDK での編集

SDK には1レイヤー128パスの既存上限がある。単体 renderer の1,000パスは負荷試験であり、そのまま一つの文書レイヤーに渡せるという意味ではない。

文書の制約に従って125パス×8レイヤー、合計1,000パスを配置した。毎回全8レイヤーの最後のパスを固有の色へ変更し、古い content-hash tile が再利用されないようにした。DPR1 / zoom1、3 warmup＋20回の編集。doc commit、SDK / Worker、可視 M5 tile、GPU 完了までを含む。画素の readback は計時外で、実際の色の更新も検査した。

| 実 SDK の観測 | 変更前 | 変更後 |
|---|---:|---:|
| シーン初回 commit → GPU 完了 | 1013.6 ms | 175.9 ms |
| 編集 → GPU 完了・中央値 | **1001.0 ms** | **134.2 ms** |
| 同 p95 | 1094.1 ms | 164.7 ms |

各1プロファイルの前後観測。統計的な採用判断は上の6プロファイルの renderer 比較で行う。初回 commit はアプリ全体のコールド起動ではない。全8レイヤーを再描画する条件であり、通常の単一パスの編集や compositor 表示の latency とは区別する。SDK の前後記録（`dense-vector-sdk-edits-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。最初の診断用 fixture は1レイヤーに1,000パスを渡して既存の入力検査に拒否された。その記録も保持し、engine の上限は緩めていない。

## 正しさ

- 10条件×5方式の RGBA16F 全バイト比較で差分0。Metal の6プロファイルと SwiftShader で一致。色の異なる半透明、0 / 微小 alpha、塗り・線、自己重なり、穴、cap / join、変形・ROI、大きな原点、8の倍数でないバッチ数を含む。
- 同じ encoder 内で ROI を変える検査、uniform の複数ページ、GPU 完了を待たない連続 submit、warm の管理量の安定、低予算で既存 scratch を退役させる検査を通過した。SwiftShader（`dense-vector-swiftshader-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。
- SDK の M6 に、密集33パスの色順・tile seam・編集・Undo/Redo・blur / mask・ZIP・GPU 再作成の回帰を追加した。変更前の配布 WASM でも同じ検査を通過し、比較条件を確認した。

## 既存回帰と配布物

- `scripts/check.sh` 成功。Rust123件、Clippy の native / WASM、SDK build / API check、Node7件、fresh release build と配布2箇所の完全一致、metadata / SHA256SUMS の照合を含む。optional vello-probe の WASM Clippy も成功した。
- SwiftShader と Apple Metal でそれぞれ94件成功：functional32、M5 8、M6 11、M7 9、M8 6、embed28。SwiftShader 回帰（`dense-vector-regression-swiftshader-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）、Metal 回帰（`dense-vector-regression-metal-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。
- Vite でビルドした minimal-embed / react-embed の描画、Undo/Redo、PNG export、保存・再読込も成功した。SwiftShader 回帰 JSON に結果を含めた。
- ラスター主体の C2 / C3・B1〜B8 は13条件を通過した。編集全体の回帰記録（`dense-vector-editor-regression-metal-2026-10-26.json`、CI artifact `alto-browser-reports`、ローカルは `verification/out/`）。密集ベクター以外の性能向上を主張する根拠にはしない。
- 配布 WASM：4,603,311 bytes、gzip1,164,303 bytes、SHA-256 `b3a7df524b5e136e420f0af47baf3a89c4e6c9cc2a690d25dce508e07da45314`。先行版から22,996 bytes増加。比較専用 WASM：6,346,465 bytes、SHA-256 `9728b2423ca0d36414068e603fc0bb49546c65b853d2c85ee3eb402d06bc60d0`。通常配布に vello はリンクしていない。

## GitHub CI

[CI run 40220872493](https://github.com/NobukiAkiyama/alto-engine/actions/runs/40220872493) は commit `ba52f250db253f2ad77683822ed8b7366592d747` の通常 check job を全て成功した。Chrome 156.0.8078.47 / SwiftShader。Rust / WASM / 配布物、回帰94件、埋め込み2例、10画素条件×5方式と寿命・予算の検査を含む。[CI の記録](../../verification/dense-vector-ci-2026-10-26.json)。手動の gpu-lifecycle 調査 job は通常 push のため未実行。

CI の配布 WASM はローカルと同じ `b3a7df52...da45314`。比較専用 WASM も CI 上でソースから再ビルドし、実行 hash は `e94aae6e...eaea826` と記録した。比較ビルドには通常配布のパス固定処理を適用していないため、実行した hash をローカルと区別する。CI を通ったソースと配布物が主作業ツリーと一致することも照合した。

通常回帰の破損 journal 復旧テスト中に `ProduceOverlay` の non-existent mailbox / Invalid mailbox を1件観測した。密集 M6 ケースより前であり、以前の `ProduceSkia` → Invalid Texture → DissociateMailboxForPresent → external Instance 消失の列は記録されていない。CI の成功を console の完全無診断や GPU 喪失の根治と取り違えず、診断行と前後の文脈も保存した。このログだけで原因や独立性を断定しない。

## 再現

```sh
sh scripts/build-m0.sh
pnpm --dir packages/alto-sdk build
sh scripts/build-vector-comparison.sh
ALTO_VECTOR_FRAMES=100 ALTO_VECTOR_REVERSE=0 \
  ALTO_BROWSER_REPORT_DIR=/tmp/dense-renderer \
  node scripts/verify-browser.mjs --vector-optimization
ALTO_BROWSER_REPORT_DIR=/tmp/dense-sdk \
  node scripts/verify-browser.mjs --suite=dense-benchmark
ALTO_BROWSER_GPU=swiftshader ALTO_VECTOR_FRAMES=1 \
  ALTO_BROWSER_REPORT_DIR=/tmp/dense-pixels \
  node scripts/verify-browser.mjs --vector-optimization
ALTO_BROWSER_GPU=swiftshader ALTO_BROWSER_REPORT_DIR=/tmp/dense-regression \
  node scripts/verify-browser.mjs --functional
sh scripts/check.sh
```

renderer の採用測定は正逆を交互に6回実行した。`dense-benchmark` は明示選択時だけ動き、通常の correctness CI には性能のしきい値を追加しない。SwiftShader の時間を実 GPU の採用値にはしない。

## 残る範囲

単体の密集1,000パスでも32.25 ms、実 SDK の全8レイヤーの変更では134.2 ms。重いシーンで常時60 fpsを保証できる速度ではない。初回 tessellation、100万パスへの拡張、他の GPU / Safari / Firefox、利用者の制作効率は未検証。Canvas / compositor の間欠的 GPU 喪失も独立調査に残る。
