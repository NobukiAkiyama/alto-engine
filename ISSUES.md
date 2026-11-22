# Open GitHub Issues — Alto Engine

> 取得日: 2026-11-22 (JST)  |  対象: [NobukiAkiyama/alto-engine](https://github.com/NobukiAkiyama/alto-engine) の未解決Issue

未解決 **34件**。内訳: P1 13件 / P2 15件 / P3 5件 / 優先度なし 1件。

## Issue一覧

### P1 — 高優先度 (24件)

- [#196 — [CodeRabbit][major] Normalize the branch name before checking object availability.](https://github.com/NobukiAkiyama/alto-engine/issues/196) **priority:p1**
- [#195 — [CodeRabbit][major] Keep the replayed edit reversible until its digest matches.](https://github.com/NobukiAkiyama/alto-engine/issues/195) **priority:p1** — Apply the record to staged state, or retain an independent rollback state and its objects. Enforce the history budget after the digest matches.
- [#191 — [CodeRabbit][major] Budget the edge visits that feathered lassos perform.](https://github.com/NobukiAkiyama/alto-engine/issues/191) **priority:p1**
- [#190 — [CodeRabbit][major] Use the updated tree to decide which text nodes were removed.](https://github.com/NobukiAkiyama/alto-engine/issues/190) **priority:p1**
- [#189 — [CodeRabbit][major] Bound the amount of outline data returned to the editor.](https://github.com/NobukiAkiyama/alto-engine/issues/189) **priority:p1**
- [#188 — [CodeRabbit][major] Adopt a successful folder publication before readback can fail.](https://github.com/NobukiAkiyama/alto-engine/issues/188) **priority:p1**
- [#187 — [CodeRabbit][major] Reject a prepared edit from another engine before releasing its stage.](https://github.com/NobukiAkiyama/alto-engine/issues/187) **priority:p1**
- [#184 — [CodeRabbit][major] Preserve the checkpoint's redo stack on divergence.](https://github.com/NobukiAkiyama/alto-engine/issues/184) **priority:p1** — Restore the original editor history on mismatch. Keep normal redo clearing when the digest matches.
- [#183 — [CodeRabbit][major] Include the font face in the layout cache key.](https://github.com/NobukiAkiyama/alto-engine/issues/183) **priority:p1**
- [#182 — [CodeRabbit][major] Do not cache an absent font blob as a parse result.](https://github.com/NobukiAkiyama/alto-engine/issues/182) **priority:p1**
- [#181 — [CodeRabbit][major] Apply the footprint budget before allocating dabs.](https://github.com/NobukiAkiyama/alto-engine/issues/181) **priority:p1**
- [#180 — [CodeRabbit][major] Preserve the channel when converting a Curves effect.](https://github.com/NobukiAkiyama/alto-engine/issues/180) **priority:p1**
- [#178 — [CodeRabbit][major] Return the arbitrary-precision number token as a JSON number in `from_slice`.](https://github.com/NobukiAkiyama/alto-engine/issues/178) **priority:p1**

### P2 — 中優先度 (15件)

- [#219 — [CodeRabbit][minor] Reject zero before starting the fuzz jobs.](https://github.com/NobukiAkiyama/alto-engine/issues/219) **priority:p2** — Require a positive duration after the digit check. Reject all-zero values such as "000" as well. Keep the default 600 valid.
- [#214 — [CodeRabbit][major] Avoid evaluating the legacy effect prefix twice per selected blur.](https://github.com/NobukiAkiyama/alto-engine/issues/214) **priority:p2** — Share same-level prefix results over an ROI that covers both requests. Keep a separate original plane when the blur samples at another level so the selection still uses the original-resolution pixels.
- [#213 — [CodeRabbit][major] Reclaim optional dense scratch before retrying under a smaller budget.](https://github.com/NobukiAkiyama/alto-engine/issues/213) **priority:p2**
- [#212 — [CodeRabbit][major] Do not skip parity when an available adapter fails initialization.](https://github.com/NobukiAkiyama/alto-engine/issues/212) **priority:p2**
- [#210 — [CodeRabbit][major] Fill the bevel region beneath the miter tip.](https://github.com/NobukiAkiyama/alto-engine/issues/210) **priority:p2**
- [#205 — [CodeRabbit][major] Release the replacement cache if scratch allocation fails.](https://github.com/NobukiAkiyama/alto-engine/issues/205) **priority:p2**
- [#201 — [CodeRabbit][major] Skip the opacity scan when the footprint exceeds 1024 tiles.](https://github.com/NobukiAkiyama/alto-engine/issues/201) **priority:p2**
- [#199 — [CodeRabbit][major] Charge packed fill geometry to the prepared-vector cache.](https://github.com/NobukiAkiyama/alto-engine/issues/199) **priority:p2**
- [#198 — [CodeRabbit][major] Share backdrop work across blur adjustments.](https://github.com/NobukiAkiyama/alto-engine/issues/198) **priority:p2** — Plan and share sibling-prefix results at each required level. Include the required halo in those results; reusing the current tile-sized accum alone would clip the blur at tile edges.
- [#197 — [CodeRabbit][major] Release the integer output buffer if composite setup fails.](https://github.com/NobukiAkiyama/alto-engine/issues/197) **priority:p2**
- [#194 — [CodeRabbit][major] Limit GC stage roots to the document being collected.](https://github.com/NobukiAkiyama/alto-engine/issues/194) **priority:p2**
- [#193 — [CodeRabbit][major] Bound the font cache and share bytes across faces.](https://github.com/NobukiAkiyama/alto-engine/issues/193) **priority:p2**
- [#192 — [CodeRabbit][major] Roll back object registration when package open fails.](https://github.com/NobukiAkiyama/alto-engine/issues/192) **priority:p2** — Stage the imported objects until validation succeeds, or roll back only the objects introduced by the failed open. Preserve objects already present for other documents or pending edits.
- [#186 — [CodeRabbit][major] Offer a structure resolution when field conflicts cannot repair the layer tree.](https://github.com/NobukiAkiyama/alto-engine/issues/186) **priority:p2** — Expose a structure resolution that covers the affected parent links and child lists. Keep ordinary field resolutions for valid trees. Apply the same fix to the stash path at Line 378.
- [#185 — [CodeRabbit][major] Refresh derived bakes without rejecting font replacement on locked layers.](https://github.com/NobukiAkiyama/alto-engine/issues/185) **priority:p2**

### P3 — 低優先度 (5件)

- [#221 — [CodeRabbit][minor] Reject reply waits when the worker fails.](https://github.com/NobukiAkiyama/alto-engine/issues/221) **priority:p3** — Reject pending waits on worker errors and a bounded deadline. Terminate the worker in failure cleanup. Preserve normal handling of started and stats replies.
- [#220 — [CodeRabbit][minor] Restore the narrow-screen sidebar width.](https://github.com/NobukiAkiyama/alto-engine/issues/220) **priority:p3** — Place the mobile override after this rule. Keep the 310px sidebar for wider screens.
- [#218 — [CodeRabbit][minor] Preserve paths containing spaces in compiler arguments.](https://github.com/NobukiAkiyama/alto-engine/issues/218) **priority:p3**
- [#217 — [CodeRabbit][minor] Invalidate the previous report when a new run starts.](https://github.com/NobukiAkiyama/alto-engine/issues/217) **priority:p3** — Clear both report values and disable Save when a run starts. Enable Save only after that run succeeds.
- [#179 — [CodeRabbit][minor] Reject negative radii before quantization.](https://github.com/NobukiAkiyama/alto-engine/issues/179) **priority:p3** — Check the sign before quantization, as the f32 converter does. Preserve acceptance of -0.0. Add the negative near-zero case to the equivalence tests.

### 優先度未設定 (1件)

- [#176 — Live brush preview shows a previous stroke's tiles because preview hashes repeat across strokes](https://github.com/NobukiAkiyama/alto-engine/issues/176) **優先度なし** — A regression test can draw two strokes on the same layer touching the same tile, then compose a display tile during the second stroke and check that it shows the second stroke's preview.

## 補足

- この一覧は取得時点でOPENのIssueです。クローズ済みIssueは含みません。
- 対応内容の詳細、再現手順、根拠は各Issue本文を参照してください。
