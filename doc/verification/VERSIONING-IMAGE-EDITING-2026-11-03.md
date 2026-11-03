# 版管理・画像編集の実装検証（2026-11-03）

対象は `/Users/mimi/Desktop/alto-engine-main` の `codex/versioning-image-editing` 作業ツリー。
基点は `aa1a3fd28977bec196f665675426792ee036b606`。本記録はローカル検証であり、CI・配布・リリース完了の記録ではない。

## 実装範囲

[設計書](../design/VERSIONING-AND-IMAGE-EDITING.md)の Phase A〜C を実装した。
未リリースのため旧形式の移行・互換読み込みは設けず、document format 5 / manifest 2 /
commit 2 / protocol 6 に統一した。render version は3を維持している。

| 領域 | 実装内容 |
|---|---|
| 文書 | 明示的なラスター画像範囲、全透明画像のサイズ保持、canonical JSON、重複キー・未知要素・不正参照の拒否 |
| 版管理 | ブランチ、保護、タグ、pin、checkout/detach、復元、DAG表示、ahead/behind |
| マージ | 三つのsnapshot比較、属性・タイル単位の統合、競合のprepare/finish、失効検出、二親commit |
| 復旧 | 履歴状態遷移のjournal、増分checkpoint、保存・再オープン、Worker停止・GPU再作成 |
| データ整理 | dry-run、全参照・Undo/Redo・処理中データの保護、保存競合検出、排他、削除途中からの再試行 |
| 画像 | 切り抜き、nearest/bilinear拡縮、90度回転、反転、グラデーション、4近傍塗りつぶし、色取得 |
| SVG | 限定した図形・パス・グループ・変形・viewBoxの型付きimport、非対応入力の一括拒否 |
| 操作面 | Rust/WASM/SDK公開API、Playgroundの操作ボタンと競合解決、追加機能検証ボタン |

切替・内容を変更するマージではUndo/Redoをリセットする。commitはUndoを保持し、復元と画像編集はUndo可能な通常編集として扱う。
画像操作はCPUを正本とし、新しいGPU高速化経路は有効にしていない。既存render v3の画素基準は変更していない。
API応答は処理適用とjournal登録までであり、永続化完了を待つ場合はSDKの `durable()` を使用する。

## 保存先ごとの範囲

| 保存先 | 確認 | 実際の整理 |
|---|---|---|
| ネイティブのフォルダ | 対応 | OS排他・manifest比較置換・中断後の再mark |
| ネイティブのZIP | 対応 | 別ファイルで検証してから排他的に原子置換 |
| SDK `MemoryObjectProvider` | 対応 | 操作直列化・失効handle拒否・manifest比較置換 |
| ブラウザFile System Access / OPFSフォルダ | 対応 | 共通排他を保証できないため `UNSUPPORTED` |
| SDKのZIP Blob | 対応 | `compactArchive` で検証済みの新しいBlobを返す |

manifest公開後に補助的な `document.json` の更新が失敗しても、保存成功を取り消さず警告を返す。
未知の参照・破損候補は、未使用と推測して削除せず中止する。

## 検証結果

最終コードでのローカル検証結果。ブラウザ試験は8スイートを通した後、追加した画面操作と保存先契約の拒否試験を含む画像編集・版管理スイートを再実行した。最終監査で効果の選択マスクにもR8実体検証を揃え、影響するengine試験62件とWASMを更新した。

| 検証 | 結果 |
|---|---|
| SDK Node tests | 44件成功、失敗0 |
| SDK API check | 通常・browser入口とも成功 |
| native clippy / WASM clippy | warningsをエラーとして成功 |
| Rust workspace tests | 初回全294件成功。Claudeレビュー対応後はengine 65件とdoc 24件を再実行して成功。PRの全体CI結果は別途確認 |
| Chrome機能・画像編集・版管理・画素一致 | 8スイート、計274件成功、失敗0（画像編集・版管理6 / 機能36 / M5 9 / M6 10 / M7 8 / M8 6 / embed 37 / parity 162） |
| Vite埋め込み例 | minimal / React両例のビルドとブラウザ起動・編集・Undo確認成功 |
| format / diff / 生成WASM一致 | 成功。SDK / PlaygroundのWASMが一致 |

再実行コマンド：

```sh
cargo test --workspace --locked -- --test-threads=1
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy -p alto-wasm --target wasm32-unknown-unknown --locked -- -D warnings
node --test packages/alto-sdk/test/*.test.js
pnpm --dir packages/alto-sdk api:check
ALTO_BROWSER_GPU=swiftshader node scripts/verify-browser.mjs --functional --suite=image-versions,m5,m6,m7,m8,embed,parity,functional
pnpm --dir examples/minimal-embed build
pnpm --dir examples/react-embed build
ALTO_BROWSER_GPU=swiftshader node scripts/verify-browser.mjs --examples
node scripts/check-build-metadata.mjs --fast
```

GPUを使うブラウザ試験とnative試験は同時実行しない。
ブラウザの生JSONと今回の検証ログは `verification/out/versioning-image-editing-2026-11-03/`（ローカル保存・gitignore対象）。新規試験は
`tests/e2e/image-version-tests.js`、native側は `image.rs` / `svg.rs` /
`versions/{branches,gc,journal}.rs` / `alto-store` の試験に含めている。

障害・境界試験には、未保存切替、未統合ブランチの削除、失効マージ、同一/別タイルの競合、
複数merge baseの拒否、誤ったjournal基点、削除途中のフォルダ再読込、ZIP置換失敗、
保存mirror更新失敗、失効storage handle、R8マスクへのRGBA誤参照、画像範囲・選択・Undo、
SVGのgroup opacityと部分import拒否、Worker停止後の復旧を含む。

保存fixtureと4つの `verification/sample*.alto` は現行形式で再生成した。
過去の配布用 `wasm/SHA256SUMS`・`verification/build.json` はリリース証跡として更新していない。

生成した本体WASMは6,143,341 bytes。両コピーのSHA-256：
`0cb4f722ff649dde194ba33244ac1ee191730ea30a4326aa59a3cd10b37cb7b1`。

## 残る範囲

- Phase DのGitブランチ・二親履歴への写像は後続。`alto-git sync` は対象履歴を明示的に拒否し、線形履歴を維持する。
- 複数merge baseの仮想統合、画素内・文字範囲の自動統合、到達可能履歴の打ち切りは含めない。
- SVGのCSS・文字・画像・外部参照、Catmull-Rom・面積縮小は非対応。
- Chrome 155 / SwiftShaderとこのMacのnative検証。実機の複数GPU・Safari/Firefox・長時間の制作運用は未検証。
- この検証はローカル作業ツリーに対するもので、公開パッケージの動作を示すものではない。

Claude Codeによるレビューと修正の記録は[こちら](CLAUDE-REVIEW-2026-11-03.md)。
