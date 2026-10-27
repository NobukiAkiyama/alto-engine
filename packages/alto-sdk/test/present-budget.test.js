import assert from "node:assert/strict";
import { test } from "node:test";
import { PRESENT_DEFER_LIMIT, countsDeferredWait, nextDeferredStreak } from "../dist/present-budget.js";

const frame = (patch) => ({ pending: 0, presentDeferred: 0, composed: 0, ...patch });

test("only an idle deferred present counts toward the cap", () => {
  assert.equal(PRESENT_DEFER_LIMIT, 40);
  assert.equal(countsDeferredWait(frame({ presentDeferred: 1 }), "wait"), true);
  assert.equal(countsDeferredWait(frame({ presentDeferred: 1, pending: 2 }), "wait"), false);
  assert.equal(countsDeferredWait(frame({ pending: 2 }), "wait"), false);
  assert.equal(countsDeferredWait(frame({ presentDeferred: 1 }), "drain"), true);
  assert.equal(countsDeferredWait(frame({ presentDeferred: 1, composed: 4 }), "drain"), false);
  assert.equal(countsDeferredWait(frame({ pending: 3 }), "drain"), false);
});

test("the streak resets when a frame makes progress and stops past the limit", () => {
  let streak = 0;
  for (let i = 0; i < 3; i++) streak = nextDeferredStreak(streak, true, 3);
  assert.equal(streak, 3);
  assert.throws(() => nextDeferredStreak(streak, true, 3), /GPU_LOST: the previous frame did not finish on the GPU/);
  assert.equal(nextDeferredStreak(9, false, 40), 0);
  let capped = 0;
  for (let i = 0; i < PRESENT_DEFER_LIMIT; i++) capped = nextDeferredStreak(capped, true, PRESENT_DEFER_LIMIT);
  assert.equal(capped, PRESENT_DEFER_LIMIT);
  assert.throws(() => nextDeferredStreak(capped, true, PRESENT_DEFER_LIMIT), /GPU_LOST/);
});
