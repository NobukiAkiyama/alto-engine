/** Idle waits of a deferred present before the Worker treats the device as lost. */
export const PRESENT_DEFER_LIMIT = 40;
/**
 * A deferred present that is not still compositing or, for `waitFrame`, not still
 * decoding. Healthy cold frames stay deferred across many ticks while `composed`
 * is non-zero; those do not count. A decode stall (`pending` only) does not either.
 */
export function countsDeferredWait(frame, where) {
    if (where === "wait")
        return frame.pending === 0 && frame.presentDeferred !== 0;
    return frame.presentDeferred !== 0 && frame.composed === 0;
}
/**
 * Counts consecutive idle deferred presents. `idle` false resets the streak.
 * The next idle present past `limit` throws `GPU_LOST` instead of waiting again.
 */
export function nextDeferredStreak(streak, idle, limit) {
    if (!idle)
        return 0;
    const next = streak + 1;
    if (next > limit)
        throw Error("GPU_LOST: the previous frame did not finish on the GPU");
    return next;
}
