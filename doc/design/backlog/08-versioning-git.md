# 版管理と Git

全項目の表と共通規則は [00-ROADMAP.md](00-ROADMAP.md)。現行仕様は [VERSIONING-AND-IMAGE-EDITING.md](../VERSIONING-AND-IMAGE-EDITING.md)。

**Goal:** 残るマージ機能、履歴の整理、同期、Git の双方向を設計する。

**Architecture:** マージは3つの完全なスナップショットの比較（`crates/alto-engine/src/versions/merge.rs`）。`alto-git` はエンジンにリンクしない CLI。

## 現状

- `prepare_merge`：共通祖先を計算し、最良が複数なら `MULTIPLE_MERGE_BASES`。
- `MAX_HISTORY_ONLY_OBJECTS = 2048`、GC は `versions/gc.rs`、遅延読み込みは `versions/lazy.rs`。
- 比較は `merge_value` による JSON 値の再帰的な 3-way。`tiles` はキーごとの値なので、別のタイルの編集は既に自動で統合される。同じタイルの編集は `pixels` の競合、範囲と画素の同時変更は `geometry` の競合になる。

## VER-1 複数の共通祖先

**設計。** 再帰マージの仮想ベース。最良の共通祖先 B1…Bn を、順に3-way で統合した仮想スナップショット V を作り、それをベースにする。

- 仮想統合で競合が出たら、競合のあるフィールドはベース同士の共通祖先の値を使う（衝突を確定させない）。それで決まらない場合のみ `MULTIPLE_MERGE_BASES`。
- V はコミットにしない。メモリ上の値で、プランの GC 根に含める。
- 計算量を抑えるため、n ≤ 8、再帰の深さ ≤ 8。超えたら `MULTIPLE_MERGE_BASES` を維持する。

## VER-2 画素内の自動マージ

**設計。** タイル単位の統合は既にある。残りは同じタイルの `pixels` 競合で、3者のタイルを復号して画素単位の 3-way を行い、両者で異なる画素だけを競合にする。全画素が統合できたタイルは競合から外す。

- `merge_value` は同期で値だけを見るので、タイルの復号は競合の収集後に別の段として行う。`geometry` の競合は対象にしない。
- 結果は通常のタイルになる。比較は常に同じ順序で、決定的である。
- 競合の解決は「ours / theirs / マスク指定」の3種。
- 画素比較は R8 / RGBA8 の完全一致で、許容差は設けない。

## VER-3 文字範囲の自動マージ

**設計。** テキストの `text` を 3-way の差分（UTF-16 の Myers diff、書記素境界に丸める）で統合する。範囲（span、縦中横、ルビ）は編集に合わせて `remap_range` で追従する。重なった編集は競合。書式は範囲を比較し、同じ範囲で値が違えば競合。

## VER-4 stash

**設計。** 未コミットの作業を、参照付きの非公開コミットに退避する。

- `refs.stashes: Vec<StashEntry { commit, message, created_at }>`。`Manifest` の `VersionRefs` に追加する。GC の根に含める。
- `doc.versions.stash(message)`：commit してヘッドを動かさず、作業を HEAD の状態に戻す。Undo 履歴はクリアする。
- `doc.versions.stashPop(index)`：3-way で適用する。競合したら merge と同じ解決に入り、完了まで stash を消さない。

## VER-5 履歴の打ち切り

**設計。** 既存の方針どおり、開いている履歴を書き換えない。新しいパッケージを別に書く。

`doc.versions.exportTrimmed({ keep: { commits?: number, since?: number }, target })`

- 打ち切り境界のコミットの親を外すと内容ハッシュが変わるので、新しいコミット列を**再計算**する。メッセージに `Alto-Trimmed-From: <hash>` を残す。
- 参照（ブランチ、タグ、pin）のうち、打ち切りで届かなくなるものは警告で返す。黙って消さない。
- 同じ親を持つマージの片親が境界外になる場合、その親を外して単親にする。

## VER-6 分散同期

**設計。** オブジェクトは内容アドレスなので、転送は Git と同じ形にできる。

- `fetch(remote)`：相手の `refs` を読み、足りないオブジェクトだけを `ObjectStorageProvider` から読む。ブランチは `remote/<name>` の別名前空間へ。
- `push(remote, branch)`：fast-forward のみ。相手の manifest の楽観ロック（`expectHash`）で競合を検出する。
- GC 中の同時 push は `gcCapabilities` の排他に従う。排他のない保存先は push を拒否する。
- サーバーを持たない。任意の共有フォルダと、STO-12 の保存先で動く。

第一段階は `fetch` と fast-forward `push` のみで、自動マージは含めない。統合は `prepareMerge` を `remote/<name>` に対して呼ぶ。

## GIT-1 Git の変更の取り込み

**設計。** `alto-git pull`：Git のコミットを Alto のコミットへ逆写像する。

- 対応は `Alto-Commit` トレーラ。トレーラのあるコミットは既存の版として飛ばす。
- トレーラのない Git コミット：ツリーのフォルダを開き、`document.json` ではなく `manifest.json` と `snapshots/`、`objects/` が完全なら、新しいコミットとして受け入れる。不足や不正なら `CONFLICT` で止め、何も書かない。
- 手で編集された `document.json` は読み込まない（写しであり正本でない）。その場合は差異を警告で返す。

## GIT-2 rebase、cherry-pick

**設計。** Alto のコミットはスナップショット全体なので、3-way マージの再利用で作れる。

- `doc.versions.cherryPick(hash)`：親を base、現在を ours、対象を theirs として 3-way し、単親のコミットを作る。競合は merge と同じ解決を使う。
- `rebase`：`cherryPick` の列。途中の競合で停止し、`continue` と `abort` を持つ。状態は文書の履歴状態遷移としてジャーナルに記録する。
- 履歴の書き換えになるので、保護ブランチでは拒否する。

## GIT-3 ブラウザ内 Git

**設計。** 設計書の候補は isomorphic-git。依存、認証、ネットワークが入るので SDK 本体に入れず、別パッケージ `packages/alto-git-browser`（`@alto/git`）として `ObjectStorageProvider` の上に乗せる。`alto-git` の `sync` と同じ写像規則（`Alto-Commit` トレーラ、ref の名前、parents の順序）を TypeScript で実装し、CLI との一致を試験する。認証は呼び出し側が渡す `onAuth` のみで、トークンを保存しない。

## 実装タスク

### Task VER-4: stash

**Files:** Modify `crates/alto-store/src/lib.rs`（`VersionRefs`）、`crates/alto-engine/src/versions/branches.rs`、`versions/gc.rs`、`versions/journal.rs`、`packages/alto-sdk/src/document.ts`。

**Interfaces:** `doc.versions.stash(message)`、`stashList()`、`stashPop(index)`、`stashDrop(index)`。

- [ ] 試験 `stash_restores_head_and_keeps_the_work`、`stash_pop_reapplies_the_work`。
- [ ] 試験 `stash_with_nothing_to_save_is_refused`。
- [ ] 試験 `gc_keeps_stashed_objects`、`stash_survives_save_reopen_and_journal_replay`。
- [ ] 試験 `conflicting_pop_keeps_the_stash`。
- [ ] 実装、PR。

### Task VER-1: 仮想ベース

**Files:** Modify `crates/alto-engine/src/versions/merge.rs`。

**Interfaces:** Produces `fn virtual_base(&self, bases: &[ContentHash]) -> Result<SnapshotValue, EngineError>`。

- [ ] 既存の `MULTIPLE_MERGE_BASES` の試験（`branches.rs:690`）を、上限超過の場合へ変える。
- [ ] 試験 `criss_cross_merges_with_a_virtual_base`、`virtual_base_conflict_does_not_reach_the_user_when_resolved_later`。
- [ ] 試験 `too_many_bases_is_refused`、`plan_keeps_virtual_base_objects_during_gc`。
- [ ] 実装、PR。

### Task VER-2: 画素の自動マージ

**Files:** Modify `crates/alto-engine/src/versions/merge.rs`、`packages/alto-sdk/src/types.ts`（競合の種類）。

- [ ] 試験 `edits_in_different_pixels_of_one_tile_merge`、`same_pixel_is_a_conflict_with_its_bounds`。
- [ ] 試験 `pixel_merge_is_deterministic_for_both_directions`（ours/theirs を入れ替えても非競合部は同じ）。
- [ ] 試験 `merge_with_lazy_objects_reads_tiles_from_the_folder`。
- [ ] 実装、PR。

### Task VER-3: 文字範囲のマージ

**Interfaces:** Consumes `remap_range`（TXT-5）。

- [ ] 試験 `edits_in_different_paragraphs_merge`、`overlap_is_a_conflict`、`merge_does_not_split_a_grapheme`。
- [ ] 実装、PR。

### Task VER-5: 打ち切り

- [ ] 試験 `trimmed_export_reopens_with_new_hashes`、`trim_reports_unreachable_refs`、`original_document_is_unchanged`。
- [ ] 実装、PR。

### Task GIT-1 / GIT-2

**Files:** Modify `crates/alto-git/src/mirror.rs`、`crates/alto-git/tests/git.rs`、`crates/alto-engine/src/versions/branches.rs`。

- [ ] 試験 `pull_accepts_a_complete_foreign_commit`、`pull_refuses_a_hand_edited_document_json`、`pull_is_idempotent`。
- [ ] 試験 `cherry_pick_makes_a_single_parent_commit`、`rebase_stops_at_a_conflict_and_aborts_cleanly`、`rebase_refuses_a_protected_branch`。
- [ ] 実装、PR。

### Task VER-6: fetch と push

**Files:** Modify `packages/alto-sdk/src/document.ts`、`storage-core.ts`、`crates/alto-engine/src/versions/branches.rs`、`crates/alto-store/src/lib.rs`（`VersionRefs` の `remotes`）。

**Interfaces:** `doc.versions.fetch(remote: ObjectStorageProvider, { name })`、`doc.versions.push(remote, branch)`。

- [ ] 試験 `fetch_reads_only_missing_objects`、`fetch_verifies_every_hash`、`remote_refs_are_gc_roots`。
- [ ] 試験 `non_fast_forward_push_is_a_conflict`、`push_loses_the_race_on_the_manifest_lock`。
- [ ] 試験 `interrupted_push_leaves_the_remote_manifest_valid`（オブジェクトが先、manifest が最後）。
- [ ] 実装、PR。

### Task GIT-3: ブラウザ内 Git

**Files:** Create `packages/alto-git-browser/`（`packages/alto-sdk` と同じく独立したパッケージ）。Modify `THIRD-PARTY-NOTICES.md`。

- [ ] Node 試験 `browser mirror produces the same commit ids as alto-git sync`（同じ入力で Git のコミット ID が一致）。作成時刻と著者を固定する。
- [ ] Node 試験 `unknown refs are a conflict`、`a re-sync writes nothing`。
- [ ] `@alto/engine` の公開物に isomorphic-git が入らないことを試験。
- [ ] 実装、PR。

## Review Focus

- 遅延読み込みの文書で、自動マージが必要な古いタイルを読めること（`ObjectTarget::Merge`）。
- マージ結果のタイルが、ジャーナル復旧で同じになること。
- マージの計算量の上限。巨大なレイヤーの画素比較でメモリを使い切らないこと。
- テキストの差分で、Undo の単位が1回であること。
- 打ち切った履歴を Git に同期したときの `alto-git` の対応表の扱い。
