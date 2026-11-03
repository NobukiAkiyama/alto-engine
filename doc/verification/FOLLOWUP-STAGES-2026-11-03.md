# 未対応機能の段階実装の検証（2026-11-03）

対象は PR #66〜#71 の積み重ね。基点は PR #65 のマージ後の main（`70ae6c0`）。結果の対象コミットと CI は各 PR を正とする。
設計は [VERSIONING-AND-IMAGE-EDITING.md](../design/VERSIONING-AND-IMAGE-EDITING.md) の第 5.2、6.6〜6.9、7.1、8.1 節。

| 段階 | 内容 | 入口 |
|---|---|---|
| 0 | 文書の整合 | README、MILESTONES、IMAGE-EDITING |
| 1 | Git Adapter Phase D | `alto-git sync` |
| 2 | キャンバス操作、スタンプ、指先、ヒストグラム、結合、ラスタライズ | `doc.canvas.*`、`doc.image.*`、Playground |
| 3 | OPFS の実 GC | `opfsFolder()`、`versions.gc({ dryRun: false })` |
| 4 | Catmull-Rom、面積平均、ベクターの画素確定 | `image.resize` の filter、`image.rasterize` |
| 5（一部） | CPU 画像編集の高速化 | 内部のみ |

## 対応状況

| 機能 | Rust | WASM | SDK | Playground |
|---|---|---|---|---|
| Git の分岐・二親マージ写像 | CLI | なし | なし | なし |
| キャンバス切り抜き・拡縮・回転・反転 | あり | あり | あり | あり |
| スタンプ、指先、ヒストグラム | あり | あり | あり | 数値入力 |
| 下レイヤーとの結合、ラスタライズ | あり | あり | あり | あり |
| Catmull-Rom、面積平均 | あり | あり | あり | あり |
| OPFS の実 GC | なし（SDK の実装） | 既存の計画 API | あり | あり |
| `sample_pixel`、`export_rgba` | あり | なし | なし | なし |

`sample_pixel` と `export_rgba` は呼び出し元のない Rust のみの関数で、効果付きの文書を扱えない。SDK の書き出しは GPU 経路を使う。

## 検証

| 検証 | 結果 |
|---|---|
| Rust workspace（直列） | 317 件成功 |
| native / WASM clippy | warnings をエラーとして成功 |
| SDK Node tests、API check | 44 件成功 |
| ブラウザ機能（SwiftShader） | 8 スイート 279 件成功（画像編集・版管理 11、機能 36、M5 9、M6 10、M7 8、M8 6、embed 37、parity 162） |
| ベクター比較（`ALTO_VECTOR_FRAMES=1`） | 10 件成功 |
| 埋め込み例 | 2 件成功 |

上の回数はレビュー修正の前半時点の実行。その後の修正を含む結果は各 PR の CI を見る。

Git の写像は実リポジトリで確認した（`crates/alto-git/tests/git.rs`、`git fsck --strict`、`git log --graph`、各 ref のツリーを新しい Engine で開く）。

```text
* after merge (HEAD -> main)
*   merge feature
|\
| * feature work (tag: alto/v1, alto/feature)
* | main work
|/
* start
* init
```

## レビュー

opencode（無料モデル）の読み取り専用レビューの指摘を、コードで確かめてから対応した。

| 指摘 | 判定 |
|---|---|
| checkout 中の Git ブランチを削除する | 修正 |
| 対応表の ref 名を信用する | 修正。写像の規則と照合 |
| Git gc で消えたコミットが恒久的な CONFLICT になる | 修正。届かない版は対応表から外す |
| ツリー内の manifest が古いブランチ名を持つ | 修正。head 以外は detached |
| `from_git` が欠損を黙って飛ばす | 修正 |
| ref 確定後の index 更新失敗で同期が失敗扱い | 修正。警告で返す |
| 同一プロセスの marks ファイルの衝突 | 修正 |
| 拡縮した選択が範囲外に薄い画素を残す | 修正 |
| unlinked mask の変形の範囲検査がない | 修正 |
| 結合が画像範囲外へ書く | 該当せず。タイルの範囲外がゼロであることは読み込みと編集で検証済み |
| 比較中に失効した排他 view が ref を書ける | 修正 |
| ロック名の取得失敗が残り続ける | 修正 |
| ロック待ちの期限が取得後の処理にも及ぶ | 該当せず。signal は待機中だけ有効 |
| R8 ラスターの刻み幅、Catmull-Rom の負値の丸め | 該当せず。ラスターは RGBA8 のみ、算術シフトで最近傍に丸まる |

## 未実装・未検証

- 段階 5：保存先からの部分読み込み、コンテンツミップマップ、画像編集の GPU 高速化は未実装。GPU は測定せず、読み戻しと符号化が残るという判断で見送った。
- 第 4 節の後続項目（複数共通祖先、画素内・文字範囲の自動マージ、stash、履歴打ち切り、分散同期、非破壊グラデーション、追加効果、ルビ等）は未着手。
- ブラウザ試験は Chrome と SwiftShader のみ。実 GPU、Safari、Firefox は未確認。
- OPFS の GC 中断は削除を失敗させて再現した。実際のタブ強制終了は試していない。
- 速度の数値は native のもので、WASM では測っていない。
- スタンプと指先の Playground は座標入力のみで、ドラッグ操作は持たない。
