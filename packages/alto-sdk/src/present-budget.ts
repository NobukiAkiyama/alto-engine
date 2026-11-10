/**
 * Idle deferred presents, after a wait that found the GPU queue empty, before the
 * Worker treats the device as lost. A present is held back only while a frame is
 * in flight, so a drained queue with the present still held back is not progress.
 */
export const PRESENT_DEFER_LIMIT = 40;
/**
 * How long idle deferred presents may keep waiting on GPU work that is still
 * running. One frame can take seconds on a software adapter or a slow integrated
 * GPU; a device that never finishes its queue is reported lost after this.
 */
export const PRESENT_STALL_MS = 30_000;

/**
 * A deferred present that is not still compositing or, for `waitFrame`, not still
 * decoding. Healthy cold frames stay deferred across many ticks while `composed`
 * is non-zero; those do not count. A decode stall (`pending` only) does not either.
 */
export function countsDeferredWait(
  frame: { pending: number; presentDeferred: number; composed: number },
  where: "wait" | "drain",
): boolean {
  if (where === "wait") return frame.pending === 0 && frame.presentDeferred !== 0;
  return frame.presentDeferred !== 0 && frame.composed === 0;
}

/** A run of idle deferred presents. */
export interface DeferredStreak {
  /** Idle presents seen while the GPU queue had drained. */
  drained: number;
  /**
   * When the run began, in `performance.now()` milliseconds. Null between runs, and
   * after an untimed queue wait resolves: the GPU finished, so the clock restarts.
   */
  since: number | null;
}

export const NO_STREAK: Readonly<DeferredStreak> = Object.freeze({ drained: 0, since: null });

/**
 * The streak after one more tick. `idle` false resets it. `gpuBusy` says the last
 * wait ended on its timer with GPU work still running: that present does not count
 * toward `limit`, since a slow frame is not a lost device, but the run as a whole
 * may not outlast `stallMs`. Past either bound this throws `GPU_LOST` instead of
 * waiting again.
 */
export function nextDeferredStreak(
  streak: DeferredStreak,
  tick: { idle: boolean; gpuBusy: boolean; now: number },
  limit: number,
  stallMs = PRESENT_STALL_MS,
): DeferredStreak {
  if (!tick.idle) return NO_STREAK;
  const since = streak.since ?? tick.now;
  const drained = streak.drained + (tick.gpuBusy ? 0 : 1);
  if (drained > limit || tick.now - since > stallMs)
    throw Error("GPU_LOST: the previous frame did not finish on the GPU");
  return { drained, since };
}
