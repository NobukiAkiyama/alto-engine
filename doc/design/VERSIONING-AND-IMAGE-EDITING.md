# 版管理の拡張と最低限の画像編集

作成日：2026-11-03／設計改訂：2026-11-03
状態：Phase A〜Dを実装。検証範囲と制約は[実装検証記録](../verification/VERSIONING-IMAGE-EDITING-2026-11-03.md)を参照。

## 1. 前提と決定事項

Alto は未リリースである。旧ファイルの読み込み、旧 SDK API、旧通信相手との
互換性は要求しない。移行処理や旧形式への書き戻しは作らず、形式不一致は
`UNSUPPORTED` とする。文書・保存容器・通信のバージョンは別々に管理する。

| 対象 | 実装前 | 採用形式 |
|---|---|---|
| 文書スナップショット | format 4 | format 5。ラスターの編集範囲を必須化 |
| パッケージ manifest | `format: 1` | `format: 2`。参照・世代を追加 |
| コミット | 明示的な形式番号なし | `format: 2`。親は最大二つ |
| SDK / Worker / WASM 通信 | protocol 5 | protocol 7。同一番号だけ接続（6 はブランチ・GC、7 でキャンバス・結合を追加） |
| レンダリング | render v3 | v3 の画素演算を維持。ラスター範囲のクリップを追加 |

RGBA8・R8、256 px タイル、BLAKE3 の内容アドレス、CPU 正本と再構築可能な GPU
キャッシュ、Transaction と可逆 Op は維持する。境界クリップは文書 format 5 の
意味として実装し、v3 の補間・合成演算そのものは変更しない。

設計着手時のコード確認結果（以下は実装前の状態）：

- `alto-doc/src/version.rs` の Commit は既に親の配列を持つ。現行検証の上限は
  16。本設計では新規形式を二親までに限定する。
- `alto-engine/src/versions.rs` にメモリ内の `prune_versions` がある。
  新規に必要なのは複数参照を起点とする到達性と、保存先の実 GC である。
- 現行 semantic diff は表示向けの要約であり、適用可能なマージ差分ではない。
- 現行 Raster はタイル集合のみを持ち、独立した画像サイズを持たない。
- 現行 storage provider に削除・共通排他の契約はない。
- 現行 `rasterize` は raster と raster の group が対象であり、SVG の vector
  出力をそのままラスタライズできるとは限らない。

### 対象と対象外

対象は Engine/WASM、SDK、保存・復旧と、それらを呼ぶ Playground の操作面。
`image` の幾何操作はレイヤー単位。キャンバス全体の操作は別の `canvas` API（第 6.7 節）。

分散編集、履歴の書き換え・打ち切り、画素の自動合成マージ、テキスト範囲の
自動マージ、HDR / 16 bit、高度な非破壊フィルタ、SVG 完全互換は対象外。
`alto-git` の参照・二親出力は独立した後続段階とする。

## 2. 文書と保存の基本モデル

履歴は不変コミットの DAG、作業文書は一つの `working`、ブランチはコミットを
指す可変参照である。Undo/Redo は working の編集履歴であり、コミット DAG と
別物である。

```text
C0 -- C1 -- C2 -------- M   main
       \              /
        F1 -------- F2     feature

M.parents = [C2, F2]
```

### 2.1 保存容器

`.alto`（ZIP）と `.altoproj`（フォルダ）は同じ論理エントリを持つ。

```text
objects/<先頭2桁>/<hash>   # タイル・アセットの既存の符号化
snapshots/<hash>.json
commits/<hash>.json
manifest.json             # 唯一の正本参照
```

フォルダの `document.json` は Git 向けの派生ミラーであり正本ではない。
`refs/heads/*` は作らない。参照の正本を manifest と二重化しない。

```text
Manifest {
  format: 2,
  document_format: 5,
  creator: string,
  snapshot: ContentHash,             // 保存時の working
  head: ContentHash | null,
  current_branch: string | null,
  branches: map<string, { head: ContentHash, protected: bool }>,
  tags: map<string, ContentHash>,
  pinned: sorted unique [ContentHash],
  generation: u64,                   // 参照更新・GC ごとに増加（working 保存の競合は snapshot と manifest token で検出）
  gc_generation: u64                 // 実 GC の開始記録ごとに増加
}
```

- branch/tag/pin/head が指すコミットと全祖先が存在しなければ破損。
- `current_branch != null` なら、その名前が存在し、その head が `head` と一致。
- `head == null` なら参照集合は空で `current_branch == null`。
- `snapshot` は head の snapshot と異なってよい。未コミットの保存を許す。
- document identity は root NodeId の UUID を使う。root NodeId と形式は履歴全体で同一。
- タグ・pin は GC の保護参照。タグの付け替えは明示的な削除と再作成にする。
- branch の保護は削除・名前変更を禁止する。通常の commit / merge による
  前進は許す。最初の `main` は保護状態で作る。

### 2.2 正規化・ハッシュと読み込み検証

GC 停止後に残る未到達コミットの欠けた親・snapshot は、到達 root の破損と区別する。
未到達断片は内容ハッシュと既知形式を検証してから次回 GC の候補にできる。

コミットと snapshot は全 object のキー辞書順、
pretty JSON と末尾 LF 一つで直列化する。欠損と null の使い分けも固定する。
数値は有限値だけを許し、負のゼロをゼロへ正規化する。snapshot はツリー順、
assets は ID 順に並べ、runtime revision、GPU キャッシュ、Undo/Redo は含めない。
Rust の直列化を正本とし、SDK が別の JSON を生成して ID を決めない。

コミット・snapshot の ID はその bytes の BLAKE3。タイル・asset は既存の
型タグ付き内容ハッシュを使う。保存済み bytes を読み込み時に別の表現へ
書き換えない。manifest の競合検出 token は実際の bytes の SHA-256 とする。

読み込みはサイズ上限・重複キー/ID・型・ハッシュ・参照先・DAG の循環・文書
グラフの整合性を検証する。未知の意味を持つフィールドやノードがあれば
通常編集・再保存・実 GC は許可しない。黙って無視して再保存しない。
必要なら別途読み取り専用の検査モードで bytes を保持する。

復旧checkpointは全体のdigest検証を前提に、内部タイルの内容ハッシュを初回復号時に照合できる。
ただしタイルの型、画像範囲との交差、境界タイルのゼロパディングは表示前に検証する。
範囲と交差しないタイルは、画素内容にかかわらず拒否する。

## 3. 版管理の操作

### 3.1 コミット

```text
Commit {
  format: 2,
  parents: [ContentHash; 0..2],
  snapshot: ContentHash,
  message: string,
  intents: [IntentSummary],
  author: string | null,
  created_at: i64,                   // Unix milliseconds
  merge_base: ContentHash | null
}
```

初回は親なし、通常は親一つ、マージは `[ours, theirs]`。
通常コミットの `merge_base` は null、マージでは選んだ共通祖先を記録する。
親の重複、自己参照、別文書の snapshot、整合しない merge_base は拒否する。

初回 commit で `main` を作る。detached 状態での commit は `CONFLICT` とし、
先にブランチを作る。ブランチの head 更新とコミット追加は一回の状態遷移。
無変更 commit は既定で拒否し、明示的な `allowEmpty` だけ許す。

### 3.2 dirty、保存、Undo の区別

- `workingChanged`：working の正規化 snapshot が head と異なること。
  head がない状態は初回 commit が必要な状態として扱う。
- `saveDirty`：working または参照状態が最後に成功した保存と異なること。
- 保存しても commit は作らない。commit してもファイルへ自動保存しない。
- API の成功は CPU 状態への適用と有効な復旧 record のキュー投入を意味する。
  復旧記録の永続化は `durable()` で待つ。commit 後は増分 checkpoint へまとめる。
  復旧を無効にしたセッションではメモリのみ。明示的 save 成功が保存先への
  永続化境界であり、UI は未保存状態を表示する。CPU 適用後の保存先/通信障害は
  適用前の計算失敗と区別し、状態と storage-failed を通知する。暗黙の再送をしない。
- 画像の確定操作と `restoreVersion` は通常の Undo/Redo 対象。
- commit、参照変更、checkout、detach、merge は Undo に積まない。
  commit は既存 Undo/Redo を維持する。checkout / detach / 内容を変える merge
  成功時は Undo/Redo と pending intent をクリアする。失敗時は維持する。

開いたストロークや未確定 Transaction がある場合、履歴操作は `BUSY` を返す。
UI/helper は先に明示的に確定またはキャンセルしてから呼ぶ。履歴操作そのものが
暗黙にストロークを確定しない。

### 3.3 API とブランチ名

| 操作 | 挙動 |
|---|---|
| `commit(options)` | working を記録し現在ブランチを前進させる |
| `restoreVersion(hash)` | head を動かさず、過去の内容を一つの可逆編集として適用 |
| `createBranch(name, { from?, checkout? })` | 既存 commit から参照を作る。checkout の既定は false |
| `checkout(name)` | workingChanged が false の場合だけ切替 |
| `detach(hash)` | 同じ制約で過去版を開き、current_branch を null にする |
| `deleteBranch(name, { force? })` | 現在/保護ブランチを拒否。未マージは force が必要 |
| `setBranchProtection(name, protected)` | 明示的に保護状態を変更 |
| `listBranches()` | head、current、protected、ahead/behind を返す |
| `createTag/deleteTag`、`pin/unpin` | GC の保護参照を操作 |
| `prepareMerge` / `finishMerge` | 第4節の二段階処理 |

`createBranch` の checkout と参照作成は一体で成功・失敗する。元 commit がない
空履歴では `NO_COMMIT`。from の既定は現在 head。復元と detach は別操作。
`saveDirty` だけを理由に checkout を拒否しない。未保存の参照変更も保持する。

ブランチ名・タグ名は NFC 正規化した 1〜128 UTF-8 bytes。先頭/末尾の空白、
先頭の `-`、制御文字、`/`、`\\`、`..`、`@{`、Git ref 禁止文字
（空白、`~ ^ : ? * [`）、末尾の `.` と `.lock` を拒否する。
大文字小文字を区別し、正規化後の同名を拒否する。OS のパスには直接使わない。

未マージ判定は、削除対象 head が現在 head の祖先（同一を含む）かどうか。
detached 中の非 force 削除は拒否する。force でも保護/現在ブランチは消せない。
最後のブランチ削除は拒否する。削除は参照だけを除去し、実データは GC まで残す。

ahead/behind は第一親距離でなく、両 head から到達可能なコミット集合の差の数。
比較先は現在 head。履歴表示は first-parent と全 DAG の明示的なモードを持つ。

## 4. 3-way マージ

### 4.1 計画と確定

```ts
const plan = await document.versions.prepareMerge("feature");
await document.versions.finishMerge(plan.token, resolutions, { message, author });
```

prepare は文書/head/Undo を変更しない。plan は ours/theirs、base、working の
revision、参照世代、内容ダイジェストに結び付いたセッション内 token を持つ。
一文書一 plan。plan の対象オブジェクトは解放まで GC の root とする。
再 prepare・cancel・close は plan を破棄する。編集や参照変更は plan を失効させる。

finish は revision、両 head、世代、解決案を再検証し、一つでも変化していれば
`STALE_MERGE`。全 conflict の解決と結果の構造検証が成功するまで状態を変更しない。
成功時は差分適用・snapshot・commit・head 更新を一つの履歴状態遷移として確定し、
通常の編集 Undo には積まない。内部の可逆 Op は原子的適用と失敗時の巻き戻しに使う。
ジャーナル復旧もこの単位で再現する。中断時に文書だけが変わった状態を残さない。

両 API は workingChanged が false であることを要求する。merge 元のブランチは
変更しない。finish の失敗時には既存の plan を保持するが、失効 token は再利用不可。

| head 関係 | 結果 |
|---|---|
| 同一、または theirs が ours の祖先 | no-op。commit は作らない |
| ours が theirs の祖先 | fast-forward。新 commit は作らない |
| 共通祖先なし | `UNRELATED_HISTORY` |
| 最良共通祖先が複数 | `MULTIPLE_MERGE_BASES`。初期実装は自動選択しない |
| それ以外 | 一つの二親 commit を作る |

最良共通祖先は、共通祖先のうち別の共通祖先の真の祖先でないもの。
第一親だけでなく全親をたどる。巡回は検出して破損エラーにする。

### 4.2 比較単位

表示用 semantic diff をパッチとして使わず、三つの完全な snapshot を比較する。
基本規則は `ours == theirs` なら採用、ours が base と同じなら theirs、逆なら
ours、それ以外は conflict。存在しない値は null 値と区別して表現する。

| 対象 | 初期の比較単位・例外 |
|---|---|
| ノードの存在・種類 | 同一 ID の異なる追加、削除対変更、種類変更対編集は conflict |
| 属性 | 名前・opacity 等はフィールド単位。transform や effect 配列は全体を一単位 |
| 親 | NodeId ごと。最終的な循環、root 移動、孤立は構造 conflict |
| 兄弟順 | 親ごとの children 配列を一単位。同一親への両側の別追加も初期は conflict |
| テキスト、ベクター、調整内容 | ノード内容全体を一単位。文字範囲・path 単位の自動統合なし |
| ラスター | タイル座標ごとの hash。欠損は透明タイルと同値に正規化 |
| マスク | 存在、transform 等の属性、R8 タイルを区別。削除対編集は conflict |
| assets | AssetId ごとの完全な記述。hash と種別も比較 |
| 文書メタデータ、選択 | メタ属性ごと、選択は全体を一単位 |

ラスターの bounds が一方で変わり、他方で同じラスターの画素が変わった場合、
初期はノード単位の geometry/content conflict にする。切り抜きと描画を自動統合
して境界外の画素を失わない。同一タイル内の別画素変更も自動合成しない。

```text
MergeConflict {
  id: string,                       // plan 内の一意 ID
  kind: existence | kind | parent | order | property | content |
        asset | pixels | geometry | structure,
  path: typed field path,
  base: { present: bool, value: value },
  ours: { present: bool, value: value },
  theirs: { present: bool, value: value }
}
Resolution { conflict_id, choice: ours | theirs | explicit, value? }
```

画素値は bytes 全体でなくタイル参照。明示値は型・参照の存在・所有文書・上限を
検証する。解決案の重複、未知 ID、未解決を拒否する。解決後に構造が不正なら
`INVALID_MERGE_RESULT` と問題パスを返し、部分適用しない。選択を変えて再試行可能。

## 5. 保存・復旧と GC

### 5.1 保存の確定点

フォルダ保存は全 immutable object を書く→内容ハッシュを検証→新 manifest を
原子的に公開→再読込検証、の順。manifest 公開が確定点。`document.json` の更新が
失敗しても正本は新 manifest であり、ミラー修復が必要な警告として返す。
確定点より前の失敗は元状態、後の応答喪失は再読込で成功/未完了を判定する。

作業の復旧 checkpoint は通常パッケージと別に、参照集合、Undo/Redo、pending
intent を保持する。journal は通常編集 record と履歴状態遷移 record を区別し、
単調な sequence・前後の状態ダイジェストを持つ。欠けた record を飛ばして再生しない。
checkpoint が保証する root を、journal/checkpoint の置換完了前に削除しない。
通常 `.alto` / `.altoproj` は Undo/Redo を保存しない。

### 5.2 排他・provider 契約

現行 ObjectStorageProvider に、保存先 ID、capability、session lease、排他実行、
manifest の原子的 compare-and-replace、冪等 deleteObjects を追加する。

- session lease は同じ保存先を開くセッション間で排他。二つ目は `BUSY`。
  読み取り専用でも遅延読込中の参照を失わないよう lease を共有せず排他とする。
  読込終了時に全 bytes を独立コピーして lease を解放する使い方は許す。
- 通常保存・参照公開・GC・同じ保存先を触る復旧・CLI は同じ排他規則に従う。
  排他範囲は immutable bytes の追加開始から manifest 公開/GC 削除完了まで。
- GC 専用 lock ファイルの存在確認だけでは不十分。OS lock、ストレージ側の
  原子的 lease 等を使う。失効した所有者の書込は fencing 等で拒否する。
- ブラウザの同一 origin の Web Locks だけを、別 origin・ネイティブ CLI まで
  含めた排他保証として扱わない。provider ごとに保証範囲を明示する。
- 排他・原子的置換・削除を保証できない provider の実 GC は `UNSUPPORTED`。
  capability を偽装する外部 writer や直接のファイル改変は保証対象外。
- Memory provider はプロセス内の排他で適合検証できる。

ブラウザの保存先は別々に評価した（`OpfsFolderProvider`、`FileSystemDirectoryProvider`）。

| 要件 | OPFS | File System Access（選択フォルダ） |
|---|---|---|
| 到達できる writer | 同一オリジンのコードのみ | 他オリジンのページ、別ブラウザ、ネイティブ CLI |
| 全 writer の排他 | 同一オリジン全コンテキストで共有される Web Lock（フォルダのパスごと） | OS lock を取れず、Web Locks は届かない |
| manifest 比較置換 | ロック内で読み取り・比較し `createWritable` の close で置換 | 比較と置換の間に外部 writer が入れる |
| 失効した処理 | 排他 view はコールバック終了で無効。保持者が死ぬとロックは解放され、死んだ文脈は書かない | 保証なし |
| 実 GC | 対応 | `UNSUPPORTED`、dry-run のみ |

OPFS の保証は SDK を経由する writer が対象で、同一オリジンのスクリプトがディレクトリを直接
変更する場合は含まない。OPFS 上のディレクトリに `FileSystemDirectoryProvider` を使っても
書き込みは同じロックに参加する。ロック待ちが上限（既定 30 秒）を超えると `BUSY`。
ブラウザのプロセス障害後は `.crswap` を残すことがあり、`list` はこれを除外する。
GC は実行のたびに現在の root から再計算し、前回の `.alto-gc.json` の候補を再利用しない。

ZIP 生成と元ファイルの置換も分ける。任意の `ArchiveTarget.write` に原子的な
置換能力があるとは仮定しない。既存ファイルの直接更新には atomic replace と
expected hash 検証を持つ専用 target が必要。それ以外は新規ファイルへの出力だけ。

### 5.3 root と意味

GC は未到達オブジェクトの回収であり、参照される古い履歴の打ち切りではない。
マージ済みブランチを削除しても、親として到達できるコミットは残る。

root は次の和集合。走査・報告は種類付き ID を重複排除する。

1. manifest の全 branches / tags / pinned / head。
2. manifest.snapshot（保存済み working）。
3. 開いている文書、Undo/Redo の正逆 Op が参照する全データ、staged objects。
4. 有効な merge plan、保存・復旧 checkpoint / journal が必要とする全データ。

コミットは全親と snapshot、snapshot/Op は全タイル・マスク・asset 等をたどる。
未知形式・解釈不能な参照、到達データの欠損/ハッシュ不一致は実削除を中止する。
候補ファイルも既知の型とハッシュで検証し、破損を未到達データとして消さない。
無関係なファイルは候補に含めない。

実 GC は saveDirty が false の時だけ許す。workingChanged は true でよい。
これにより未コミットの保存を root として保護できる。live Undo 用のデータも
GC 実行中は root に追加するが、Undo を通常パッケージの永続履歴へ昇格させない。
メモリ GC と保存先 GC は別処理とし、両方が同じ到達性規則を使う。
既存 `prune_versions` は単一 head 基準のまま公開せず、複数 root 基準へ置換する。

### 5.4 フォルダ GC

1. session lease と共通の排他を取得。未完了 GC を検査して現在の manifest を読む。
2. root を固定し、必要なデータを保存先へ追加・検証。到達集合と削除候補を作る。
3. manifest hash・root digest・候補パス/hash/bytes を含む GC record を一時書込
   から原子的に公開する。全候補と全 root の検証完了前には削除しない。
4. `generation` と `gc_generation` を増加した manifest を CAS 公開し再検証する。
   working、head、参照集合は変えない。GC record に実行中状態を記録する。
5. 候補だけを冪等に削除。エントリ単位で中断可能で、全体の原子的削除とは呼ばない。
6. 全 root を再検証し GC record を完了にする。lease を解放する。

manifest hash 不一致なら削除前に `CONFLICT`。排他を保持した削除途中の障害は
回収漏れだけを残し、到達データを失わない。クラッシュ後は共通排他を取り直し、
現在の root から到達性を再計算する。古い候補一覧をそのまま再実行しない。
古い record の処理を開始時に終えるまで新しい GC record で上書きしない。

### 5.5 単一 ZIP と報告

ZIP は到達オブジェクトだけを新しい一時 ZIP に書き、CRC に加えて内容ハッシュ、
全参照、DAG、manifest を検証する。その後 expected hash と排他を確認して
atomic replace。元ファイルを先に切り詰めない。置換前の中断では元ファイルが
有効、置換後では新ファイルが有効。backup 保持は target の明示的な保存方針。

`gc({ dryRun: true })` が既定。report は root 数、commit/snapshot/object 別の
保持/削除件数・bytes、基準 manifest token、完全性を返す。bytes は保存上の
エントリ payload の長さで、ZIP ヘッダ等を含む実ファイル縮小量とは区別する。
実行権限のない保存先でも読取りが安定していれば dry-run は可能。不安定なら
`CONFLICT`、未保存なら saveDirty を報告する参考値とし実行用 token を出さない。

実削除は `dryRun: false` の明示指定だけ。dry-run token があっても実行時に再計算し、
状態が変わっていれば `CONFLICT`。途中失敗は処理済み数を伴うエラーを返す。

## 6. ラスターの範囲と画像操作

### 6.1 format 5 の Raster

```text
Raster {
  bounds: { x: i32, y: i32, width: u32, height: u32 },
  tiles: TileMap<RGBA8>
}
```

bounds はノードローカルの整数画素・半開区間。幅/高さは 1〜65536、面積は
64 × 1024 × 1024 画素以下。右端・下端はタイルの外周も i32 に収まるよう 2147483392 以下に制限する。
新規の空ラスターは作成時に範囲を指定し、helper の既定はキャンバス範囲。
canvas は画像とは別の既存座標上限を維持する。既定の画像範囲が上限を超える
canvas では、レイヤー作成時に画像範囲を明示する。全透明でも bounds は残る。

bounds 外は透明。境界タイルの範囲外 bytes はゼロにし、全透明タイルは格納しない。
描画・blur 等の確定画素操作は必要に応じて bounds を拡張するが、上限超過は
全操作を失敗させる。幾何操作以外も新しい規則を使う。bounds だけを拡張しても
過去に切り落とした画素は復活しない。effect による見かけの広がりは別の描画 bounds。

不変条件は SetTiles、ReplaceNode、import、journal replay でも検証する。
タイル外周を丸めたサイズから画像サイズを推測しない。bounds は semantic diff、
node digest、保存、merge、Undo の対象とする。

### 6.2 API・座標・原子性

```ts
document.image.crop(id, rect)
document.image.resize(id, width, height, { filter: "bilinear" })
document.image.rotate(id, quarterTurns)
document.image.flip(id, "horizontal" | "vertical")
document.image.gradient(id, options)
document.image.fill(id, point, color, { tolerance, selection })
document.image.pickColor(id, point)
document.image.pickComposite(documentPoint)
document.image.importSvg(svg, { parent, index })
```

crop/resize/rotate/flip/gradient/fill は同じ NodeId に対する一つの可逆 Transaction。
pick は読み取り専用、SVG は vector/group の追加 Transaction であり、ラスター限定の
例外。確定 helper は外側の transact へ暗黙に入れ子にしない。高度な一括編集には
prepare で可逆低レベル Op と staging token を作り、明示的 transact へ渡す。
staging token は文書/revision に結び付け、GC root として管理する。

画像 API の引数はノードローカル座標。点を使う操作は `floor(x), floor(y)` の画素、
補間や gradient の評価は画素中心 `(x+0.5,y+0.5)`。非有限値は拒否する。
crop は整数 rect だけを受け付け、SDK は暗黙に丸めない。
文書座標 helper は祖先を含む逆変換を使う。非可逆変換はエラー。回転・shear 下の
文書矩形をローカルの軸平行矩形へ黙って置換しない。

確定操作は CPU reference を先に実装する。対象 revision を固定し、入力検証→一時
出力/逆 Op 作成→上限検証→一回の適用。計算失敗・cancel・device loss・revision
不一致では文書/Undo/head を変えず、一時データを解放する。GPU 化は正本出力の
bytes 一致が確認できた操作だけ。GPU readback 後に hash を確定する。

画像の出力に加え入力、mask、作業バッファ、Undo の保持コストを予算化する。
64 × 1024 × 1024 画素以内でも実行予算を超えれば `RESOURCE_LIMIT`。メモリ不足を入力不正と
混同しない。親を含む locked は変更不可。スポイトは locked を読める。

### 6.3 幾何操作

| 操作 | 範囲と画素の規則 |
|---|---|
| crop | rect は現在 bounds に完全包含する正の矩形。新 bounds を rect にし、範囲外を除去。原点詰め直しなし、transform は不変 |
| resize | 左上 `(x,y)` を固定して指定幅/高さへ変更。中心写像 `srcX=x+(dstX-x+0.5)*oldWidth/newWidth-0.5`、Y も同様 |
| rotate | quarterTurns は 1/2/3。正は画面座標で時計回り。左上を固定し、奇数回は幅/高さを交換 |
| flip | bounds 不変。左右なら `u→width-1-u`、上下なら `v→height-1-v` |

90度の画素写像は旧 bounds 内の `(u,v)` を `(height-1-v,u)` へ送る。
幾何操作は transform/opacity/effect 自体を焼き込まず、画素だけを確定変換する。
crop は残した画素の文書上の位置を変えない。キャンバス bounds も変更しない。

resize の filter は nearest、bilinear（既定）、catmull_rom、area。未対応は `UNSUPPORTED`。
v3 の整数 sampler の座標量子化・丸めを CPU と共有する。premultiplied で補間し、出力は既存の
straight RGBA8 へ戻す。端は旧 bounds 内の最端画素へ clamp。alpha=0 の RGB は 0。

- catmull_rom：Q8 の小数位置から得る整数重み（軸ごとの和 `2 * 256^3`）の 4×4 畳み込み。
  overshoot は alpha を 0〜1 に、色を 0〜alpha に clamp する。
- area：新しい画素の下にある元画素の面積を厳密な有理数の重みで平均する。大幅縮小の alias を
  避ける。bilinear の大幅縮小は alias が出る。

どちらも浮動小数を使わないので、環境に依存しない。GPU を通らず画素基準には関わらない。

### 6.9 大きな画像の速度

4096×4096（16.8M 画素）のレイヤーの準備時間（Apple Silicon、native release、
`cargo run --release -p alto-engine --example image_bench`）。出力は変更前後で同じ bytes。

| 操作 | タイル参照を保持する前 | 保持後 |
|---|---|---|
| 反転 | 1050 ms | 165 ms |
| 90 度回転 | 1070 ms | 160 ms |
| 拡縮 nearest 1/2 | 294 ms | 79 ms |
| 拡縮 bilinear 1/2 | 979 ms | 179 ms |
| 拡縮 catmull-rom 1/2 | 3714 ms | 619 ms |
| 拡縮 area 1/2 | 1175 ms | 285 ms |
| グラデーション | 1686 ms | 621 ms |

GPU 化は有効にしていない。残る時間の大半は画素ごとの整数演算とタイルの符号化で、GPU で計算しても
CPU 正本の bytes とハッシュを得るための読み戻しと符号化が残る。bytes 一致を確認できた経路で
読み戻し込みの測定がこの数値を下回るまで、CPU のままとする。

幾何操作は文書の選択範囲を使わず画像全体へ適用する。既存の選択は文書座標の
まま保持する。マスクは次のように扱う。

- crop：ノードの入力範囲を切る。mask の座標写像は不変で、全逆データを Undo で保持。
- resize/rotate/flip：旧ローカルから新ローカルへの境界座標の affine を H とする。
  linked raster mask の `mask_transform` は `H * old` に更新し mask bytes は再補間
  しない。unlinked mask は文書上の写像を維持する。
- ノードローカルの vector mask / clip path は H を適用し、参照点と曲線も更新する。
  変換後の上限を検証し、対応できない構造は部分適用せず `UNSUPPORTED`。

### 6.4 選択と色の共通規則

gradient/fill の selection は `"current" | "ignore"`、既定 current。
選択なしは全面、選択ありで全ゼロなら `EMPTY_SELECTION`。
文書の R8 選択をノードの画素中心から world mapping し v3 の sampler で評価する。
0 は変更/探索不可、1〜255 は被覆率。選択を操作開始時の revision に固定する。

Color は `srgb-straight` の有限な `[r,g,b,a]`、各 0〜1 として明示する。
既存 RGBA8 は straight sRGB として decode/encode する。gradient の色補間は
sRGB transfer で linear に変換して premultiplied 補間、その後 sRGB へ戻す。
既存 v3 の合成方式を変更するものではない。量子化と transfer の CPU 実装を正本に
し、GPU 最適化に先立ち固定 test vector を用意する。

gradient/fill の書込は既存 v3 の normal source-over とし、opacity と選択被覆率を
source alpha に掛ける。erase/replace は提供しない。透明色の書込や同じ結果は
no-op とし、Undo や revision を増やさない。これらは bounds を拡張しない。

### 6.5 gradient、fill、pick

Gradient は linear/radial、ノードローカルの start/end、2〜16 stops、opacity。
start/end が同一点、範囲外 offset、不正値は拒否する。offset は安定ソートし、
重複は拒否する。0/1 に stop がなくても端の色を延長する。linear は射影、radial は
start からの距離/end までの半径。パラメータを 0〜1 へ clamp する。

fill は CPU の4近傍 scanline flood fill。開始点と比較して、RGBA8 の RGB を
`round(channel * alpha / 255)` した premultiplied bytes と alpha の各差が
すべて tolerance 以下の画素を探索する。直前に訪れた画素との差ではない。
tolerance は整数 0〜255、既定0。開始点が bounds 外は `NOT_FOUND`、選択0なら
`EMPTY_SELECTION`。探索は変更前の画素を読む。最大16 × 1024 × 1024 訪問画素、
4096の異なるタイルを上限とし、超過で全体を失敗させる。透明な欠損タイルも
bounds 内では探索対象。ギャップ閉じは含めない。

pickColor は効果・マスク・opacity を適用する前の指定レイヤーの正本 RGBA8 を
Color と bytes で返す。bounds 内の欠損は透明、bounds 外は `NOT_FOUND`。
pickComposite は別 API とし、文書整数画素を zoom/pan/背景チェッカーに依存しない
scale 1 の合成結果から読む。読み戻しの unpremultiply/量子化規則を固定し、文書
revision と render 完了を確認する。キャンバス外は `NOT_FOUND`。

### 6.6 スタンプ、指先、ヒストグラム

`image.cloneStamp(id, source, stroke, { opacity, selection })` と
`image.smudge(id, stroke, { strength, selection })` は同じ NodeId への一つの Transaction。
stroke はノードローカルの折れ線（1〜4096 点）と丸い先端（半径 1〜1024、hardness 0〜1）。
点の間は半径の 1/4 間隔（最小 1 画素）で補う。bounds を拡張せず、bounds 外は読み書きしない。
どちらも gradient と同じ selection 規則に従い、選択と重ならなければ `EMPTY_SELECTION`。

スタンプの参照元は編集前の画素で固定する。自分の出力を再度写さず、重なった dab は画素ごとの
最大被覆率で一度だけ normal source-over する。指先は先頭の dab の画素を持ち運び、
dab ごとに premultiplied で補間して、持ち運ぶ色も結果へ更新する。処理量の上限は
合計 64M 画素・dab と作業メモリ `EDIT_BUDGET`。超過は全体を `RESOURCE_LIMIT` にする。

`image.histogram(id, range?)` は読み取り専用。range は bounds 内に限り、既定は bounds 全体。
効果・マスク・opacity を適用する前の格納値を数える。範囲内の透明画素はタイルの有無に
かかわらず 0 として数え、`pixels` は常に範囲の画素数と一致する。

### 6.7 キャンバス全体の操作

`canvas.crop(rect)`、`canvas.resize(w, h)`、`canvas.rotate(q)`、`canvas.flip(axis)` は
レイヤー単位の `image` とは別の文書操作で、一つの Undo になる。旧座標から新座標への
affine H をルートノードの transform に左から掛け、`meta` の幅と高さを更新する。

- 全ノードは祖先として H に従う。タイル、bounds、ローカルのベクター、テキスト、linked mask、
  効果の R8 は変更しない。hash も変わらないので作業メモリを使わない。
- crop の矩形はキャンバスの外へ出てもよく、範囲外の内容は失わず見えなくなるだけ。
- 文書座標で持つものだけを書き換える。選択範囲は再配置（resize は v3 と同じ Q8 の
  bilinear で再標本化、4096 タイル上限）。unlinked mask は `mask_transform` に H を掛ける。
- resize で Gaussian の半径（文書 px）を同じ倍率にする。非等倍、または半径 64 超は
  `UNSUPPORTED` で全体を拒否する。
- 画素は表示時に v3 のサンプラで変形する。resize は確定リサンプルではなく、繰り返しても劣化しない。
  幅高さ 1〜65536 の範囲外と不正な回転数は `INVALID_ARGUMENT`。
- 旧 `edit.rs` の実装（タイル再配置）は、テキスト位置の喪失、タイルの無い場所への描き込みの欠落、
  マスクの未対応があり、呼び出し元も無かったため置き換えた。

### 6.8 下レイヤーとの結合とラスタライズ

`image.mergeDown(id)` は下の兄弟ラスターへ確定する。`image.rasterize(id)` はラスターの
整数平行移動を画素へ確定し、ラスターだけのグループを同じ NodeId の一枚へ置き換える。
どちらも CPU の Q14 normal source-over で一つの Transaction。

見た目を保てない構造は部分適用せず `UNSUPPORTED`：マスク・ベクターマスク・クリッピングパス・
効果、クリッピングの基底になる層、非表示の層、Normal 以外の合成モード、整数画素でない
相対位置、ラスター以外の子、opacity か合成モードを持つ pass-through グループ。
結合で下層の opacity は画素へ焼き込み、結果の opacity は 1。グループの置換では
グループ自身の属性（opacity、合成モード、マスク、効果、変形）を保持する。

## 7. SVG 限定インポート

SVG は型付きの vector/group を追加する一つの Transaction。XML 全体を検証し、
一つでも非対応ならノードを追加しない。NodeId は成功予定の追加ごとに発行し、
SVG の id を文書の ID として再利用しない。

- ルート svg、g、path、rect（rx/ry含む）、circle、ellipse、line、polyline、polygon。
- fill/stroke の単色/none、fill-opacity、stroke-opacity、stroke-width、linecap、
  linejoin、miterlimit、fill-rule、opacity、transform。
- path の絶対/相対命令と円弧。円弧は誤差上限0.01ローカル単位の cubic へ分割し、
  変換後も座標・命令数上限を検証する。
- root width/height は正の unitless/px、viewBox は有限で正寸法。少なくとも
  viewport 寸法または viewBox を必須とし、欠損寸法は viewBox から補う。
  preserveAspectRatio は既定 xMidYMid meet と none のみ。
- transform は型付きノード変換として保持する。親子の style 継承は importer が
  解決する。既定 fill=black、stroke=none。色は #RGB/#RRGGBB と black/white のみ。
  g opacity は子の alpha に配らず isolated group の opacity にする。

script、DTD/外部実体、外部URL、href/use、style/CSS、filter、mask、pattern、
gradient、image、text、ネストした svg、%/em 等の単位、未知要素/属性は拒否する。
コメント、XML宣言、SVG namespace 宣言だけは明示的に読み飛ばせる。
エラーは要素/属性名と UTF-8 byte offset、行/列を含む。

入力 UTF-8 は最大4 MiB、要素は最大10000、XML深さは最大64。さらに既存 M6 の
ノード/path/命令/座標/線幅/ツリー深さ上限を変換後に適用し、厳しい方を採る。

vector のラスタライズは `image.rasterize` の明示操作だけで行う（第 7.1 節）。

### 7.1 ベクターからの画素確定

| 項目 | 定義 |
|---|---|
| 対象 | Vector ノード、および第 6.8 節の条件を満たすラスターとベクターのグループ |
| 計算 | renderer が v3 で使う CPU 整数コード（`vector_pixels_q14`：map_path、rasterize、rasterize_stroke、source_over_q14）。GPU を読み戻さない |
| 解像度・変形 | ノードのローカル画素に 1:1。world が整数平行移動のときだけ受け付け、それ以外は `UNSUPPORTED`。拡縮や回転の下で確定すると見た目が変わるため |
| 範囲 | 全パスの制御点と線幅（miter 分を含む）を覆う bounds。キャンバス外の形も失わない |
| 透明度・合成・マスク・効果 | 単体の Vector では焼き込まず同じノードの属性として残す。意味は同じ。グループの子は第 6.8 節のとおり拒否 |
| 出力 | premultiplied Q14 から straight RGBA8。不透明部は一致。半透明部は量子化を一度余分に通るため ±1 の差があり得る |
| メモリ | タイル数 × 256 KiB が `EDIT_BUDGET` を超えると `RESOURCE_LIMIT` |
| 座標範囲外 | 表示は透明で終わるが、形を失わないよう `UNSUPPORTED` |

## 8. SDK、UI、Git の境界

`DocumentHandle.versions` に履歴操作、`image` に画像操作を置く。
protocol は明示的 handshake を持つ。provider の GC 能力は `gcCapabilities`
で返す。画像編集は CPU 正本であり、任意の GPU 高速化を導入する場合は別途 capability
を追加する。Engine と SDK で型とエラーコードを共有し、
SDK だけの検証で成功扱いにしない。新形式から旧形式への fallback はしない。

代表結果は判別可能な型とする。

```text
MergePlan = NoOp | FastForward | ThreeWay { token, ours, theirs, base, conflicts }
MergeResult = NoOp | FastForward { head } | Committed { commit, base }
ImageEditResult { node, bounds, changed_tiles, no_op }
BranchView { name, head, current, ahead, behind, protected }
GcReport { baseline, complete, roots, by_kind, dry_run, warnings }
```

Playground は「保存」「版を記録」「過去版の内容を復元」「ブランチ切替」を別操作で
表示する。過去版から分岐する操作は createBranch + checkout の原子 API を使う。
切替時の未コミット拒否と Undo クリアを事前表示する。merge は競合一覧と全解決後の
確定操作を持つ。GC は dry-run 結果と明示的な実行を分ける。

`alto-git` の写像は第 8.1 節（Phase D）。Git import/rebase/cherry-pick は含めない。

### 8.1 Git Adapter の写像（Phase D）

`alto-git sync <folder> [--branch <name>] [--ref-prefix <name>]` は、ブランチ・タグ・pin・
head から到達する全コミットを親から順に一つずつ Git コミットへ写す。

| Alto | Git |
|---|---|
| コミット | 1 コミット。parents は同じ順序。ルートの親は初回同期時の対象ブランチ先端（無ければなし） |
| 最初に対応付けるブランチ（現在ブランチ、無ければ `main`、無ければ名前順の先頭） | `--branch` または checkout 中のブランチ（無ければ `main`） |
| その他のブランチ `x` | `refs/heads/<prefix>/x`（`prefix` の既定は `alto`） |
| タグ `x` | `refs/tags/<prefix>/x`（軽量タグ） |
| pin、detached の head | `refs/alto/<文書ID>/keep/<Altoコミット>` |
| 対応表 | `refs/alto/<文書ID>/state` が指す blob（JSON、`format: 1`） |

文書 ID は root NodeId。対応表は Alto コミット→Git コミット、各 ref の最後の値、基点、
フォルダの位置を持つ。一度決めたブランチ先と prefix は固定で、別の指定は `CONFLICT`。

同期は読み取りだけの検証→import→ref の一括更新の順に進む。

1. 対応表の全コミットについて、`Alto-Commit` トレーラ、parents の順序、ツリー内の
   コミットファイルの内容ハッシュを確認する。不一致は `CONFLICT`、未知の形式は
   `UNSUPPORTED`。対応表が無いのに同じ版が既に Git にあれば、再 import せず `CONFLICT`。
2. 各 ref が記録値のままか確認する。手動のコミット・移動・削除はすべて `CONFLICT` で、
   上書きも先端への積み増しもしない。未対応の ref が既にあれば同じく拒否する（初回の
   対象ブランチだけは基点として受け入れる）。Alto 側のブランチが記録値の子孫でない場合も
   `CONFLICT`。Git ref を巻き戻さない。
3. 他の文書が使う ref、大文字小文字だけが異なる ref は拒否する。
4. `git fast-import` は `refs/alto/<文書ID>/staging` にだけ書く。その後
   `git update-ref --stdin` で全 ref を期待値付きで一括更新し、staging を消して対応表を
   置き換える。途中で失敗しても公開 ref と対応表は元のまま。再実行で同じ結果になる。

各 Git コミットのツリーはその Alto コミットの祖先だけを含む。マージは第一親のツリーに
第二親側の履歴を足す。これにより任意の Git リビジョンのフォルダが単体で有効な
パッケージになる。manifest はブランチ先端と head ならその祖先内の参照だけを持ち、それ
以外は detached で開く。未統合ブランチがある間、作業フォルダのその分のファイルは
checkout 中の Git ブランチから見て未追跡になり、manifest も差分になる。これは
作業フォルダが全ブランチを持つことの結果で、同期は作業フォルダを変更しない。

Alto で削除したブランチ・タグの ref は、記録値のままで同期が作ったものだけ削除する。
初回より前からあった Git ブランチと、外部で動いた ref は残し、警告を返す。

## 9. 実装順序と検証ゲート

### Phase A：形式と原子的な状態遷移

1. format 5 / manifest 2 / commit 2 / protocol 6 と canonical fixtures。
2. ラスター bounds、全描画・編集・import 経路での境界不変条件。
3. branches/tags/pins、DAG検証、status、checkout/detach/restore。
4. 履歴状態遷移 journal/checkpoint、保存・再オープン・device loss 復旧。

### Phase B：merge と GC

1. 三つの snapshot 比較、構造検証、prepare/finish と stale 検出。
2. 共通 root 列挙とメモリ GC、storage capability/lease/CAS/削除契約。
3. folder dry-run → 障害注入済み実 GC。
4. ZIP の検証済み新規出力 → 対応 target だけ atomic replace。

### Phase C：画像編集と操作面

1. CPU crop/rotate/flip/nearest/bilinear、mask/selection、逆 Op。
2. gradient/fill/pick、SVG importer と拒否ケース。
3. SDK API、Playground、保存・復旧・merge との結合。
4. 必要な操作だけ GPU 化。CPU bytes と一致しない経路は有効化しない。

### Phase D：Git Adapter

参照と二親出力の対応、Git ref 競合、再 sync の冪等性を独立検証する（第 8.1 節、`crates/alto-git/tests/git.rs`）。

各段階で `cargo test --workspace --locked -- --test-threads=1`、SDK API check、Node tests、該当する
ブラウザ機能試験を通す。WASM export 変更時は両配布 pkg を再生成し、SDK と
Playground が同じ生成物を使うことを確認する。GPU 試験は直列に実行する。

| 領域 | 必須の境界・障害試験 |
|---|---|
| 形式 | 旧番号拒否、未知要素拒否、同一状態の同一bytes/hash、非有限値、重複ID/キー |
| 履歴 | 初回、保存とcommitの違い、未コミット切替拒否、restoreのUndo、保護/force、detached |
| merge | 同一/片側変更、no-op/FF、複数base、削除対編集、順序/循環、同一/別タイル、geometry対画素 |
| merge確定 | stale token、無効解決値、journal途中停止、確定後再オープン、部分適用なし |
| GC | 全参照/Undo/staging/plan root、未知参照/破損で中止、保存競合、古い候補の再利用禁止 |
| 障害注入 | 各公開/置換/削除境界の直前直後で停止し再実行。失効leaseの書込拒否 |
| 幾何 | 全透明bounds、負の原点、1px、非256倍、タイル境界、奇数回転、linked/unlinked mask |
| 色/選択 | alpha0、半透明、部分選択、回転した親、bilinear端、CPU/GPU bytesとtile hash一致 |
| fill | seed基準、透明欠損、4近傍、画素/タイル上限超過で全失敗 |
| SVG | 成功fixture、group opacity、viewBox、円弧、外部実体/URL/未知属性/過大入力の拒否 |

## 10. 完了条件

- 新形式のみを扱い、旧形式や未知の意味を黙って読み替えない。
- 保存済み未コミット状態、参照、二親 DAG を両保存容器から再オープンできる。
- 切替、復元、commit、merge と Undo の関係が第3・4節どおりになる。
- 未解決/失効した merge は文書/head/Undo を一切変えない。
- 実 GC 後と停止後の再試行で、全 root の bytes/hash が保たれる。
- 排他を保証できない provider は実 GC を拒否する。
- 画像操作が bounds、mask、選択、Undo/Redo、保存、復旧、commit/diff/merge に接続する。
- CPU reference の結果を正本とし、GPU 有効化には一致の根拠がある。
- SVG の非対応入力は部分 import せず、位置付きエラーで拒否する。
- API check、保存/復旧試験、該当ブラウザ試験を通し、Playground 上で操作を確認する。

将来の別設計：履歴打ち切り、stash、分散同期、
複数baseの仮想マージ、画素/文字範囲の自動マージ、面積縮小/Catmull-Rom、
高度なフィルタ、非破壊gradient、vectorラスタライズ、SVGのCSS/画像/文字。
