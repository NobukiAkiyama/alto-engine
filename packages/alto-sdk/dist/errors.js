const CODES = [
    "INVALID_ARGUMENT",
    "CONFLICT",
    "NOT_FOUND",
    "CORRUPT_DATA",
    "GPU_LOST",
    "GPU_UNSUPPORTED",
    "STORAGE",
    "INTERNAL",
    "WORKER_CRASHED",
    "WORKER_UNAVAILABLE",
    "WORKER_TIMEOUT",
];
/**
 * A failure the engine reports. `message` is an English explanation for developers;
 * words shown to people come from the host, keyed by `code` and `details`.
 */
export class AltoError extends Error {
    code;
    details;
    constructor(code, message, details, options) {
        super(message, options);
        this.name = "AltoError";
        this.code = code;
        this.details = details;
    }
}
/** The Worker sends `{code, message}`; an unknown code is a bug, reported as INTERNAL. */
export function fromWorkerError(error) {
    const code = CODES.find((c) => c === error?.code) ?? "INTERNAL";
    return new AltoError(code, error?.message || code, conflictDetails(code, error?.message));
}
/** Engine messages name some conflicts in their text; carry them as a reason. */
function conflictDetails(code, message = "") {
    if (code !== "CONFLICT")
        return undefined;
    if (message.includes("document-closed"))
        return { reason: "document-closed" };
    if (message.includes("no-recovery"))
        return { reason: "no-recovery" };
    if (message.includes("recovery-pending"))
        return { reason: "recovery-pending" };
    if (message.includes("active tool"))
        return { reason: "tool-active" };
    if (message.includes("revision"))
        return { reason: "revision" };
    return undefined;
}
