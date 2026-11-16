# 版を `.alto` の外に持つ

作成日：2026-11-16。状態：実装済み（ブランチ `claude/versions-outside-alto`、結果は [ENGINE-HARDENING-RESULTS.md の F-9](../verification/ENGINE-HARDENING-RESULTS.md#f-9版を-alto-の外に持つ)）。対象：main `bcf8ab5`。

## 1. 目的

`.alto` は、保存のたびに全ての版のタイルを含める。版は描き直す前のタイルを持ち続けるので、版を作るほど `.alto` が大きくなり、512 MiB の上限（`MAX_PACKAGE_BYTES`）に届くと保存できなくなる。堅牢化計画の F-2 で、固定 seed の4時間試験が140分で上限に届き、原因は漏れではなく版の持つタイルだと分かった。F-4〜F-7 は commit・保存のメモリと OPFS を減らし、上限に近づいたことを知らせるが、`.alto` の大きさは変えていない。

この設計では、過去の版のタイルを `.alto` の外（版の置き場）に書き、`.alto` には作業文書と版の一覧だけを入れられるようにする。これで、版の数が増えても `.alto` は作業文書の大きさに近いままになる。

## 2. 目標と対象外

目標：

- `.alto` の大きさが版の数に比例しない保存の仕方を用意する。
- 版の置き場がつながっていなくても、作業文書と版の一覧（log、diff、status）は開ける。
- 保存、開く、復旧、GC のどれでも、版のタイルを黙って失わない。

対象外：

- 既定の `save(target)` の振る舞いは変えない（全ての版を含める）。新しい保存は明示オプションにする。失敗の現れ方が変わるため（`ON-DEMAND-HISTORY-OBJECTS.md` の判断と同じ）。
- 作業文書のタイルを外に出すこと（表示範囲に応じた読み込みは STO-1）。
- 版の置き場を自動で選ぶこと、ファイルの横に自動で作ること。置き場はホストが渡す。
- 512 MiB の上限そのものを変えること。

失敗とみなすこと：

- 版の置き場が別の文書のものや古いものでも気づかず、版に戻ったときに違う画素になる。
- `.alto` を開けたのに、版の一覧が置き場の有無で変わる。

## 3. 今の仕組み（使うもの）

- **フォルダ形式（`.altoproj`）**：`saveFolder(provider)` で、`.alto` と同じ論理エントリを1ファイルずつ書く。上限はない。同じ置き場への2回目以降の保存は、無いオブジェクトだけを書く。GC は manifest の参照から到達するものを残す（`VERSIONING-AND-IMAGE-EDITING.md` 5.4）。
- **必要な分だけ読んで開く**：`openFolder(provider, { history: "on-demand" })` は作業文書のタイルだけを読み、過去の版のタイルは使う操作の直前に読む。置き場に在ると分かっているオブジェクトの集合（known）と、文書が参照してメモリに無いもの（external）を区別する。ページを読み直した後は `versions.attachFolder(provider)` でつなぎ直す。復旧のチェックポイントは external の中身を持たず、`lazy-objects.json` に known を書く（`ON-DEMAND-HISTORY-OBJECTS.md`）。
- **版なしの保存**：`save(target, { history: "omit" })` は作業文書だけの `.alto` を書く。版の一覧も含まない。

## 4. 方法の比較

| 方法 | 内容 | 長所 | 短所 |
|---|---|---|---|
| A（採用） | `.alto` に作業文書のタイルと版の一覧を入れ、過去の版のタイルはフォルダ形式の置き場に書く。開くときは置き場をつなぎ、版のタイルは必要時に読む | `.alto` の使い方を壊さない。「必要な分だけ読む」の仕組み、`saveFolder` の差分書き込み、フォルダ GC をそのまま使える | `.alto` と置き場の2つを一緒に扱う必要がある |
| B | 版を持つ文書はフォルダ形式だけにし、`.alto` は版を持たない受け渡し用にする | 新しい形式がほぼ要らない | 今 `.alto` で版を使っている流れが使えなくなる |
| C | 上限を上げる・なくす | 変更が小さい | 版の中身は増え続け、保存のメモリと WASM の 4 GiB の壁が残る。原因に効かない |

利用者は作り方を任せたので、A を選んだ（私の判断）。

## 5. 設計

### 5.1 用語

- **薄い `.alto`**：過去の版のタイルを含まない `.alto`。作業文書のタイル、全てのスナップショットと commit の JSON、manifest、外に置いたオブジェクトの一覧を含む。
- **版の置き場**：同じ文書の `.altoproj` 形式のフォルダ（`ObjectStorageProvider`）。過去の版のタイルを持つ。普通のフォルダ形式と同じもので、作業文書のオブジェクトも含む完全なパッケージなので、単独で `openFolder` しても開ける。作業文書のオブジェクトは薄い `.alto` と置き場の両方に入る（置き場を単独で正しいパッケージにするため。重複は作業文書の大きさだけ）。

### 5.2 薄い `.alto` の中身

```text
manifest.json            # format 3。external_history を持つ
snapshots/<hash>.json    # 全て（今と同じ）
commits/<hash>.json      # 全て（今と同じ）
objects/<2>/<hash>       # 作業文書が参照するものだけ
external-objects.json    # 外に置いたオブジェクトのハッシュ（辞書順の配列）
```

manifest に1つ加える：

```text
Manifest {
  format: 3,                      // 薄い .alto だけが 3。普通の .alto とフォルダは 2 のまま
  ...今と同じ...,
  external_history: {
    document: <root NodeId>,      // 置き場が同じ文書かを確かめる
    objects: <件数>,
    digest: <external-objects.json の BLAKE3>
  }
}
```

- 作業文書が参照するオブジェクトは必ず `.alto` に入れる。`external-objects.json` は、版だけが参照するオブジェクトの集合と一致しなければならない（多くても少なくても破損）。
- `.alto` の中で、全ての参照は「`.alto` の中に在る」か「`external-objects.json` に在る」のどちらか。どちらでもなければ今と同じく破損。

### 5.3 保存

`doc.save(target, { history: { folder: provider } })` を加える。

1. 編集キューの中で、全オブジェクトを使える状態にする（今の `save` と同じ `withObjects({ type: "all" })`）。
2. 置き場に `saveFolder(provider)` と同じ処理で書く。無いオブジェクトだけを書き、manifest を最後に書いて確定する。ここで失敗したら `.alto` は書かない。
3. 置き場の確定が終わってから、薄い `.alto` を作って `target` に書く。中に入れるのは作業文書のオブジェクトだけなので、`.alto` の上限と保存のメモリは作業文書の大きさで決まる。
4. 成功したら、文書を保存済みにし、置き場を文書につなぐ（`setFolder`）。以後の版のタイルは、今の「必要な分だけ読む」と同じく置き場から読み、メモリから外せる。

- 置き場がつながっていない文書（置き場なしで開いた薄い `.alto` など）を別の置き場へ保存するには、外にある版のタイルが要る。読めなければ今と同じく `STORAGE`（reason `objects-unavailable`）で、何も書かない。
- 開いている間は、今の `openFolder` の on-demand と同じく置き場の共有ロック（`holdObjects`）を持ち、GC に消されないようにする。

`packageSize`（F-6）は、薄い `.alto` で保存するときの大きさを返すよう、オプションを受ける（`packageSize({ history: "external" })`）。

### 5.4 開く

`engine.open(source, { history: provider })` を加える（`provider` は省略可）。

1. `.alto` を読み、manifest が format 3 なら（今の読み手は format 2 以外を `UNSUPPORTED` にするので、読み手を 2 と 3 の両方を受けるように変える） `external-objects.json` を読んで、5.2 の規則で検証する。
2. `open_entries_on_demand(entries, known)` と同じ経路で開く。known は `external-objects.json` の集合。作業文書のタイルは `.alto` から読む。
3. `provider` があれば、置き場の manifest の root NodeId が `external_history.document` と同じことと、`external-objects.json` の全てが置き場の `objects/` に在ることを確かめる。満たせば `setFolder` でつなぐ。満たさなければ開くのは成功させ、置き場はつながず、警告 `HISTORY_STORE_UNAVAILABLE`（新設。理由：`missing`、`other-document`）を返す。
4. 置き場がつながっていない間、版のタイルが要る操作は今と同じく `STORAGE`（reason `objects-unavailable`）。`versions.attachFolder(provider)` でつなげる（3. と同じ確認を加える）。

### 5.5 復旧と GC

- 復旧：今の「必要な分だけ読む」と同じ。チェックポイントは external の中身を持たず、`lazy-objects.json` に known を書く。ページを読み直した後は `attachFolder` が要る。
- 置き場の GC：置き場の manifest から到達するものだけを残す。薄い `.alto` は保存のたびに置き場の manifest も更新する（5.3 の手順 2）ので、最後に保存した薄い `.alto` の版は置き場に残る。それより前に保存した薄い `.alto` の版は、その後に版を整理（prune）して GC すると置き場から消えることがある。その `.alto` を開くと、消えた版は `objects-unavailable` になる（作業文書は開ける）。開くときの確認（5.4 の 3.）で気づけるようにする。
- 薄い `.alto` を普通の `.alto` に戻す：置き場をつないだ状態で `save(target)`（既定）を呼ぶと、全てを含む format 2 の `.alto` になる（今の振る舞い）。

### 5.6 SDK の変更

- `DocumentHandle.save(target, { history: "all" | "omit" | { folder: ObjectStorageProvider } })`。
- `Engine.open(source, { history?: ObjectStorageProvider })`。
- 警告コード `HISTORY_STORE_UNAVAILABLE`。
- `packageSize({ history?: "all" | "external" })`。

## 6. 移行方針

- リリース済みの形式はまだない（タグは0件）。`RELEASING.md` と未実装ロードマップの方針どおり、旧形式からの移行は実装しない。
- 普通の `.alto` とフォルダの形式（manifest format 2）は変えない。今の `.alto` は今までどおり読み書きできる。
- 薄い `.alto` だけが manifest format 3 になる。format 3 を知らない読み手は、今の検証どおり未知の形式として開かない（黙って版を失った文書として開かない）。
- 文書の形式（document_format 5）、描画（render v3）は変えない。画素は変わらない。

## 7. 試験

- 単体：薄い `.alto` の書き出しと読み込み。`external-objects.json` の過不足、digest の不一致、作業文書のオブジェクトの欠け、別の文書の置き場を、それぞれ破損または警告として扱う。置き場なしで開いて log・diff・status が使えること、版に戻ると `objects-unavailable` になること。
- ブラウザ：薄い `.alto` で保存し、置き場ありで開き直して版に戻り、普通の `.alto` と同じ画素になる。置き場なしで開いてから `attachFolder` で続けられる。タブを終わらせて復旧する（クラッシュ試験に1点加える）。
- 計測：F-2 と同じ長時間試験を、薄い `.alto` の保存で動かし、`.alto` の大きさが版の数に比例しないこと、4時間で上限に届かないことを確かめる。F-5 のベンチで、保存のメモリが作業文書の大きさで決まることを確かめる。

## 8. 実装する人に任せること

- `external-objects.json` を manifest の中に入れるか別エントリにするか（大きくなりうるので別エントリを提案した）。
- 置き場への書き込みと薄い `.alto` の組み立てを、Worker のどの要求に分けるか。
- 警告の details の中身。
