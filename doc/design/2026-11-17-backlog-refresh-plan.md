# 未実装機能の設計書の書き直し 作業計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `doc/design/backlog/` の13本と `00-ROADMAP.md` を、main `bdf26a9` のコードのまま実装に着手できる細かさに書き直す。

**Architecture:** 領域ごとに1タスク。調査役が今のコードを読んで1本を書き直し、機械検査（パスと名前の実在、6点の有無）と opencode レビューを通して1コミットにする。最後に ROADMAP で文書間の番号と順序を揃える。

**Tech Stack:** Markdown。調べる対象は Rust（`crates/`）、WGSL、TypeScript SDK（`packages/alto-sdk/`）、試験（`tests/`）。

**Spec:** [2026-11-17-backlog-refresh.md](2026-11-17-backlog-refresh.md)

## Global Constraints

- コード・試験・設定は1行も変えない。変えるのは `doc/design/backlog/*.md` だけ（ROADMAP を含む）。
- 基点は main `bdf26a9`。各文書の冒頭に「基点：main `bdf26a9`（2026-11-17 書き直し）」と書く。
- 日本語。既存文書の文体（である調、短い文、表）に合わせる。
- 各項目に6点：**現状／11-04 からの変化／設計／形式と通信／作業手順／依存**。見出しはこの名前で。
- パスと型・関数名は main に実在するものだけ書く。新しく作るものには「（新規）」を付ける。
- 除外の8項目（UI-1〜5、SEL-4、AI-2、AI-3）は「実装しない」を変えない。6点は不要で、事実だけ直す。
- 共通規則（ROADMAP「共通規則」1〜8）は各項目の要件に暗黙に含まれる。重ねて書かない。
- 測定で形が変わる項目は判定条件を残す。速度の約束を書かない。
- 利用者の決定が要る点は決めない。案と既定を書く。
- シェーダーのモード番号・`format_version`・`PROTOCOL` の値は各文書では「要割り当て」と書き、Task 14 で一括して決める。

## Review Focus

- 書かれたパスが古い（移動・改名済み）のに「現状」に載る → 機械検査で落とす。
- F-9（版を `.alto` の外に持つ、manifest format 3、`external_history`、`.altoproj`）と食い違う保存系の設計 → 08、09 の reviewer に必ず渡す。
- 堅牢化 A〜F で入った上限・エラーコードと矛盾する上限 → 各文書で `doc/verification/ENGINE-HARDENING-RESULTS.md` と照合。
- 2文書が同じ型・ファイルに別の変更を設計している（例：`Effect` を 02 と 07 が拡張）→ Task 14 で表にして解く。
- 「11-04 からの変化：変化なし」と書いたのに、関係ファイルが 11-04 以降に変わっている → 検査で `git log b2036ce..bdf26a9 -- <path>` を確かめる。

---

### Task 0: 検査の準備

**Files:**
- Create（スクラッチパッド、リポジトリ外）: `check-doc.mjs`

- [ ] **Step 1:** `check-doc.mjs <doc>` を書く。文書中の `` `crates/…` `` `` `packages/…` `` `` `tests/…` `` `` `scripts/…` `` `` `doc/…` `` 形式のパスを抜き出し、「（新規）」が同じ行に無いのに `bdf26a9` に存在しないものを列挙する。各 `## <ID>` 節に6つの見出しがそろっているかも列挙する。
- [ ] **Step 2:** 今の `09-storage-export.md` に当てて、欠けが出ること（6見出しが無い）を確かめる。

### Task 1〜13: 領域ごとの書き直し

1タスク＝1文書。順序：13（検証）→ 04 → 03 → 07 → 05 → 06 → 02 → 01 → 08 → 09 → 10 → 12 → 11。小さく独立したものから進め、保存系（08、09）は F-9 の文脈を持たせて後半に置く。

各タスクの手順は同じ。

- [ ] **Step 1:** 調査役に、対象の文書・Spec・本計画の Global Constraints を渡して書き直させる。関係するコードを読み、項目ごとに6点を書く。既存の「実装タスク」節は、各項目の「作業手順」に移して消す。
- [ ] **Step 2:** `node check-doc.mjs doc/design/backlog/<file>` を実行。出力が空になるまで直させる。
- [ ] **Step 3:** 「11-04 からの変化」が「変化なし」の項目について、関係パスの `git log --oneline b2036ce..bdf26a9 -- <paths>` を見て、本当に変化なしか確かめる。
- [ ] **Step 4:** opencode の読み取り専用レビュー（`opencode run --agent plan -m opencode/deepseek-v4.1-flash-free "<prompt+diff>" < /dev/null`）。指摘はコードで確かめてから直す。
- [ ] **Step 5:** コミット `doc: refresh backlog <NN> against bdf26a9`。利用者に1行で報告。

### Task 14: ROADMAP の統合

**Files:** Modify `doc/design/backlog/00-ROADMAP.md`

- [ ] **Step 1:** 13本の「要割り当て」を集め、シェーダーのモード番号（34 から）、`format_version` と `PROTOCOL` を上げる項目とまとめる単位を表にして決める。各文書の「要割り当て」をその値に置き換える。
- [ ] **Step 2:** 2文書が同じファイル・型を変える箇所を表にし、順序か担当を決める。
- [ ] **Step 3:** 全項目の対応表（段階・工数）と「実装の順序」を更新する。変えたものには理由を書く。冒頭の基点を `bdf26a9` に。
- [ ] **Step 4:** 末尾に「計画外の候補」（PDF/SVG 書き出し、OS フォント、Issue #13 など）を列挙する。
- [ ] **Step 5:** 全14本に `check-doc.mjs` を当て、空であること。`grep -c "要割り当て" doc/design/backlog/*.md` がすべて 0。
- [ ] **Step 6:** opencode レビュー、コミット `doc: reconcile backlog roadmap`。設計書 Spec の状態を「完了」に更新して同じコミットに入れる。
