import assert from "node:assert/strict";
import { test } from "node:test";
import { NO_STREAK, PRESENT_DEFER_LIMIT, PRESENT_STALL_MS, countsDeferredWait, nextDeferredStreak } from "../dist/present-budget.js";

const frame = (patch) => ({ pending: 0, presentDeferred: 0, composed: 0, ...patch });
const idle = (now, gpuBusy = false) => ({ idle: true, gpuBusy, now });

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
  let streak = NO_STREAK;
  for (let i = 0; i < 3; i++) streak = nextDeferredStreak(streak, idle(i), 3);
  assert.equal(streak.drained, 3);
  assert.throws(() => nextDeferredStreak(streak, idle(3), 3), /GPU_LOST: the previous frame did not finish on the GPU/);
  assert.deepEqual(nextDeferredStreak({ drained: 9, since: 0 }, { idle: false, gpuBusy: false, now: 1 }, 40), NO_STREAK);
  let capped = NO_STREAK;
  for (let i = 0; i < PRESENT_DEFER_LIMIT; i++) capped = nextDeferredStreak(capped, idle(i), PRESENT_DEFER_LIMIT);
  assert.equal(capped.drained, PRESENT_DEFER_LIMIT);
  assert.throws(() => nextDeferredStreak(capped, idle(PRESENT_DEFER_LIMIT), PRESENT_DEFER_LIMIT), /GPU_LOST/);
});

test("waits on GPU work still running do not count, but the run is capped in time", () => {
  // A 5 s frame on a software adapter: 100 timer-ended waits of 50 ms.
  let streak = nextDeferredStreak(NO_STREAK, idle(0), PRESENT_DEFER_LIMIT);
  for (let i = 1; i <= 100; i++) streak = nextDeferredStreak(streak, idle(i * 50, true), PRESENT_DEFER_LIMIT);
  assert.deepEqual(streak, { drained: 1, since: 0 });
  // A device that never finishes its queue is still reported lost.
  assert.equal(PRESENT_STALL_MS, 30_000);
  streak = nextDeferredStreak(streak, idle(PRESENT_STALL_MS, true), PRESENT_DEFER_LIMIT);
  assert.throws(() => nextDeferredStreak(streak, idle(PRESENT_STALL_MS + 1, true), PRESENT_DEFER_LIMIT), /GPU_LOST/);
  // The time cap holds for drained waits too, and progress restarts the clock.
  assert.throws(() => nextDeferredStreak({ drained: 1, since: 0 }, idle(PRESENT_STALL_MS + 1), PRESENT_DEFER_LIMIT), /GPU_LOST/);
  streak = nextDeferredStreak({ drained: 1, since: 0 }, { idle: false, gpuBusy: true, now: PRESENT_STALL_MS }, PRESENT_DEFER_LIMIT);
  assert.deepEqual(nextDeferredStreak(streak, idle(PRESENT_STALL_MS + 1, true), PRESENT_DEFER_LIMIT), { drained: 0, since: PRESENT_STALL_MS + 1 });
  // A resolved untimed queue wait clears `since`; the count carries on.
  assert.deepEqual(nextDeferredStreak({ drained: 3, since: null }, idle(50_000, true), PRESENT_DEFER_LIMIT), { drained: 3, since: 50_000 });
  assert.ok(Object.isFrozen(NO_STREAK));
});
