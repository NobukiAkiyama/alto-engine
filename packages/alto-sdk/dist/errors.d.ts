/** What kind of failure an `AltoError` reports. Hosts branch on this, not on `message`. */
export type ErrorCode = "INVALID_ARGUMENT" | "CONFLICT" | "NOT_FOUND" | "CORRUPT_DATA" | "GPU_LOST" | "GPU_UNSUPPORTED" | "STORAGE" | "INTERNAL" | "WORKER_CRASHED" | "WORKER_UNAVAILABLE" | "WORKER_TIMEOUT";
/** Why a `CONFLICT` happened. */
export type ConflictReason = "tool-active" | "folder-changed" | "folder-occupied" | "another-tab" | "namespace-in-use" | "recovery-pending" | "not-failed" | "revision";
/**
 * Values that let a host tell cases of one code apart. Which keys appear depends on
 * the code: `reason` for `CONFLICT` and `INVALID_ARGUMENT`, `node` for `NOT_FOUND`.
 */
export interface ErrorDetails {
    reason?: ConflictReason | "namespace" | "cross-origin-assets" | "scale" | "no-recovery";
    node?: string;
    namespace?: string;
}
/**
 * A failure the engine reports. `message` is an English explanation for developers;
 * words shown to people come from the host, keyed by `code` and `details`.
 */
export declare class AltoError extends Error {
    readonly code: ErrorCode;
    readonly details: ErrorDetails | undefined;
    constructor(code: ErrorCode, message: string, details?: ErrorDetails, options?: {
        cause?: unknown;
    });
}
/** The Worker sends `{code, message}`; an unknown code is a bug, reported as INTERNAL. */
export declare function fromWorkerError(error: {
    code?: string;
    message?: string;
} | undefined): AltoError;
