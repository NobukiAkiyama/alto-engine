# エンジンの修正・検証：結果

計画：[2026-11-09-engine-hardening-plan.md](../design/2026-11-09-engine-hardening-plan.md)。

| 段階 | 状態 | 実行コミット | 環境 | 記録・PR |
|---|---|---|---|---|
| A | 成功 | `0e6f491` | 下記 | [A](#a基準となる検証) |
| B | 成功 | `1b5ed04` | 下記 | [B](#bcpu合成経路の削除) |
| C | 未着手 | | | |
| D | 未着手 | | | |
| E | 未着手 | | | |
| F | 未着手 | | | |

## 環境

macOS 27.2、Apple M5（Metal 4）、rustc 1.99.1、Node v22.23.2、Chrome 155.0.8059.84（headless、Graphiteは既定のバックエンド）。

## A：基準となる検証

2026-11-09、`0e6f491` で実行。作業ツリーの変更は文書だけ。

| コマンド | 終了コード | 結果 |
|---|---|---|
| `sh scripts/check.sh native` | 0 | fmt・clippy成功。Rustテスト28群・337件成功、失敗・無視0 |
| `sh scripts/check.sh wasm-fast` | 0 | WASMの同梱コピーが一致。SDKのビルド・API検査・単体テスト成功 |
| `node scripts/verify-browser.mjs --functional` | 0 | 8群・284件成功（下表）。予期しないGPUプロセスのクラッシュ0件 |

| ブラウザ試験 | 件数 | 結果 |
|---|---|---|
| image-versions | 13 | 成功 |
| functional | 37 | 成功 |
| m5 | 9 | 成功 |
| m6 | 10 | 成功 |
| m7 | 8 | 成功 |
| m8 | 6 | 成功 |
| embed | 37 | 成功 |
| parity | 164 | 成功 |

前回のレビューで中断された「GPU等のテスト群」を含め、全体が中断なしで完走した。既存の失敗はなかった。

## B：CPU合成経路の削除

`crates/alto-engine/src/edit.rs` を削除し、`Engine::export_rgba` と `Engine::sample_pixel` を取り除いた。

- 削除の根拠：workspace・SDK・WASMのいずれにも呼び出し元がない。Rustクレートは未リリースで互換性の約束がない（`RELEASING.md`）。SDKの書き出しはGPU経路、スポイトは `Engine::pick_color` を使い、どちらもこのコードに依存しない。
- これで解消した欠陥：大きな平行移動を `as i32` で黙って飽和させていたこと、親子の累積と画素−オフセットをi32で計算していたこと（debugでpanic、releaseで折り返し）、タイル形式の検査がない画素読み取りの重複。
- `edit.rs` にあったテスト `overlay_reaches_the_renderer_and_levels_bump_the_format` はこのメソッドを使っておらず、`scene_tests.rs` へ内容を変えずに移した。
- 描画の独立したCPU参照（`crates/alto-gpu/src/cpu_composite/`、`tests/e2e/cpu-composite.js`）は残した。

検証（`1b5ed04`）：

| コマンド | 終了コード | 結果 |
|---|---|---|
| `sh scripts/check.sh native` | 0 | fmt・clippy成功。Rustテスト28群・337件成功（移したテストを含む。件数はAと同じ） |
| `sh scripts/check.sh wasm-fast` | 0 | SDKのビルド・API検査・単体テスト成功。WASMの同梱コピーは一致。本体のSHA-256はAの `cb39675d…` から `c3ff1199…` に変わった（コード削除による想定どおりの変化。基準ハッシュは更新していない） |

ブラウザ試験はBでは再実行していない。変更は呼び出し元のないRustメソッドの削除で、WASMとSDKの公開APIは変わらない（`api:check` 成功）。

opencode（deepseek-v4.1-flash-free）のレビューは3点を挙げ、いずれも修正不要と判断した。import不足の指摘は誤り（`lib.rs` のimportが `use super::*` で届き、テストは成功）。公開APIの削除は未リリースのため問題ない。書き出し時のガードの消失は意図どおり。

過去の設計文書（`2026-10-24-embedding-api.md` など）と [FOLLOWUP-STAGES-2026-11-03](FOLLOWUP-STAGES-2026-11-03.md) に残る `export_rgba` / `sample_pixel` / `edit.rs` への言及は、当時の記録としてそのままにした。
