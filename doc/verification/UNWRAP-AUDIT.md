# ファジングと unwrap / expect の監査（QA-1、2026-11-08）

設計：[13-verification.md](../design/backlog/13-verification.md) の QA-1。基点は main `fe87225`。

## 数え方

`crates/` の `.rs` から、次を除いて `.unwrap()` と `.expect(` を数えた。

- `tests/`、`examples/`、`benches/` の下、`*_tests.rs`、`tests.rs`
- `#[cfg(test)]` 以降の行
- `#[cfg(test)]` または `#[cfg(feature = ...)]` で宣言されたモジュール（`alto-gpu` の `cpu_composite/` 全体、`alto-wasm` の `vector_probe.rs`）

設計書の「約270」はテストを含む数だった。製品コードは **71箇所**。

| crate | 箇所 |
|---|---|
| alto-engine | 31 |
| alto-gpu | 25 |
| alto-core | 5 |
| alto-doc | 4 |
| alto-decode | 2 |
| alto-text | 2 |
| alto-store | 1 |
| alto-vector | 1 |

## 判定

外部のバイト列（`.alto`、復旧ジャーナル、スナップショット、コミット、SVG、フォント、パス）の値で失敗するものを欠陥とした。直前の検査や構築で成り立つ不変条件に頼るものは、理由を記録して残す。

**外部入力で失敗するもの：0箇所。** ファジング（下記）で見つかった3件は、どれも `unwrap` / `expect` ではなかった。

| 分類 | 箇所 | 例 | 成り立つ理由 |
|---|---|---|---|
| 固定長・定数 | 16 | `text_bake.rs` の `take(4)?.try_into()`、`alto-decode` のヘッダー、`selection.rs` の R8 タイル、`glyph_q8.rs` の4隅の min/max、`json.rs` の `from_f64(0.0)` | 直前の切り出しや固定長の配列が長さを決める |
| 構成上必ずある値 | 11 | `image/stroke.rs` の `expect("just inserted")`、`journal.rs:104,106` の一時エンジンの文書、`gc.rs` と `journal.rs` の `split(..).next()` | 同じ関数内で入れた値か、空にならない反復 |
| 版の対応表 `versions[h]` | 8 | `branches.rs`、`merge.rs` の `self.versions.get_mut(&h)`、現在のブランチの `get_mut` | 呼び出しの冒頭の `self.versions(h)?` と分岐名の検査。現在のブランチの存在は `refs.validate` が読み込み時に保証 |
| 履歴グラフの数え上げ | 3 | `versions.rs:325,337`、`branches.rs:90` | `validate_graph` 済みで、親は祖先集合に必ず含まれる |
| `head.unwrap()` | 1 | `branches.rs:264` | `refs.validate` が「現在のブランチがあれば head がある」を保証 |
| 保存済みの `manifest.json` | 1 | `gc.rs:229` | 同じ関数の `prepare_gc` が無ければ `CORRUPT_DATA` で返す |
| 3者マージの根 | 2 | `merge.rs:205,266` | 文書の根は常に JSON オブジェクトで、3者がそろう根の合成は値を返す。衝突の経路は根より下 |
| 検証済みの値の直列化 | 5 | `digest.rs`、`version.rs:40`、`alto-store` の manifest、`alto-vector` のハッシュ | 有限の数と文字列だけを持つ型（非有限の数は検証で拒否） |
| 検証済みの引数 | 8 | `selection.rs` のぼかし半径・投げ縄、`paint.rs` の羽根と線、`params.rs` の半径、`stroke_q8.rs` の段数 | 文書の検証か呼び出し側で範囲を確かめた値 |
| エンジン内部の状態 | 16 | `readback.rs` の `Mutex::lock()`（8）、`vector.rs` のアトラスとページ、`paint.rs` のパイプライン、`lib.rs:170` のタイル段 | 外部入力に依らない。リリースは `panic = "abort"` で、Mutex の poison は起きない |

合計 71。

## ファジング

`fuzz/`（`cargo +nightly fuzz`、workspace の外）。8 target、各 60 秒、1入力 10 秒・RSS 2 GiB を上限に、Apple M 系の macOS で実行した。debug assertion と overflow 検査は有効（cargo-fuzz の既定）。

| target | 入口 | 実行回数 | 結果 |
|---|---|---|---|
| `package` | `decode_package`、`Engine::open_package` | 41,087 | 問題なし |
| `recovery` | `read_entries` → `decode_recovery_entries` | 68,212 | 問題なし |
| `snapshot` | `Engine::open_snapshot` | 590,205 | 問題なし |
| `commit` | `Commit::parse`、`parse_history` | 1,406,086 | 問題なし |
| `svg` | `Engine::prepare_svg_import` | 510,538 | 問題なし |
| `path` | `PathData::from_svg` | 3,779,709（最終の修正後） | 1件を修正 |
| `font` | `inspect`、`FontData::new` | 1,339,544（修正後、120 秒） | 2件を修正 |
| `journal` | `Engine::replay_record`（`sample-m6.alto` に適用） | 15,159 | 問題なし |

### 見つかった欠陥

| # | target | 入力 | 症状 | 修正 |
|---|---|---|---|---|
| 1 | `path` | 半径 1e107 で一周近くを回る円弧（`A`、large-arc） | kurbo は円弧を `ceil(n * 掃引角 / 2π)` 個の3次曲線に分ける（`n = (1.1163 * 半径 / 0.1)^(1/6)`）。この入力は約 1e18 個になり、3.7 GB を確保しようとして落ちる。SDK のパス文字列や SVG の取り込みから、エンジンの Worker を止められた | `PathData::from_svg` が kurbo の前に `svgtypes` で走査する。各円弧は kurbo が実際に使う半径（`Arc::from_svg_arc`、端点に届くまで拡大した後）から同じ式で曲線数を見積もり、合計が `validate` の上限 4096 命令を超えるか、端点が座標上限 ±1048576 の外なら拒否する |
| 2 | `font` | 面の数 0x6ca00000 を名乗る 21 バイトの TTC | ttf-parser が面の表の長さを先に計算し、自身の debug assertion で止まる。リリースでは `None` で拒否されるので実害はないが、ファザーが先へ進めない | `check_collection` で TTC の表がファイルに収まることを先に確かめる（`inspect`、`FontData::new`） |
| 3 | `font` | `sTypoDescender` が -32768 のフォント | `-d as f64` が i16 のまま符号を反転して溢れる。debug ではパニック、リリースでは符号が戻らず descent が負になる | 広げてから反転（`-(d as f64)`） |

再現入力は `tests/fixtures/fuzz/<target>/` に置き、`crates/alto-engine/tests/fuzz_regressions.rs` の `fuzz_regressions_do_not_panic` が通常の試験で読む。

### 実行方法

```bash
sh fuzz/run-all.sh 60
```

```bash
TARGETS="path font" sh fuzz/run-all.sh 600
```

週1回、`.github/workflows/fuzz.yml` が各 target を 10 分ずつ、日付を固定した nightly（`FUZZ_TOOLCHAIN`）で実行する（PR では実行しない）。失敗した target の再現入力は artifact `fuzz-crash-<target>` に、コーパスは actions cache（次回へ引き継ぐ）と artifact `fuzz-corpus-<target>` に保存する。

各回の入口は毎回新しいエンジンを作る。SVG の取り込みなどは新しいノード ID を作るので、ID に依る欠陥は再現しない可能性がある。見つかった欠陥はどれも ID に依らなかった。

### レビュー

opencode（deepseek-v4.1-flash-free、読み取り専用）の指摘のうち、次を反映した。

- 円弧：kurbo は端点に届くまで両方の半径を同じ倍率で拡大する。生の半径だけを上限と比べる最初の修正では、扁平な楕円で倍率が大きくなる場合を見落とす。拡大後の半径から曲線数を数える形に改めた（指摘の入力 `A 1000000 1e-100` は、半径 1e-5 以下を直線として扱う kurbo の規則で実害はなかった）。
- フォント：`FontData::new` は 64 MiB の上限を通らないので、TTC の表の長さを 32 ビットの範囲でも確かめる。
- `run-all.sh`：target が無いときやシードの複製に失敗したときに 0 で終わらない。workflow：再現入力とコーパスを別の artifact にし、cache の鍵に target と再実行回数を入れ、秒数の入力は環境変数で渡して数字だけを受け付ける。

反映しなかったもの：`decode_recovery_entries` へ空の集合を渡すと復号に届かない、という指摘は誤り（集合は不透明なオブジェクトの検査にだけ使い、空なら全エントリを通常どおり復号する）。

### 検証

- `sh scripts/check.sh native`：rustfmt、全試験、clippy が通過。
- `node scripts/verify-browser.mjs --functional`：functional 37、M5 9、M6 10、M7 8、M8 6、画像編集 13、embed 37、parity 164 がすべて通過（parity の固定ハッシュは不変）。
