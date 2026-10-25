/**
 * Hooks for the engine's own tests: crashes, fault injection, damaged journals.
 * Not covered by the version promise. Product code must not use it.
 */
import type { Engine } from "./engine.js";
export declare function testing(engine: Engine): {
    /** Any Worker request by name. */
    request: (t: string, payload?: Record<string, unknown>) => Promise<unknown>;
    /** Restarts the Worker as a crash would. */
    crash: () => Promise<void>;
    destroyGpu: () => Promise<unknown>;
    /** The module URL the next Worker start loads. */
    setWorkerURL: (url: URL | string) => void;
    pendingRequests: () => number;
    /** Generation, record count, object count and pack count of the recovery store. */
    recoveryStore: () => Promise<{
        latestGeneration: number;
        records: number;
        objects: number;
        packs: number;
    }>;
    corruptJournalTail: () => Promise<unknown>;
    corruptCheckpoint: () => Promise<unknown>;
    corruptCheckpointCrc: () => Promise<unknown>;
    corruptBothCheckpoints: () => Promise<unknown>;
    /** Writes a format 1 checkpoint, as an older engine did. */
    legacyCheckpoint: () => Promise<unknown>;
    /** Writes a format 3 checkpoint (one file per object). */
    looseCheckpoint: () => Promise<unknown>;
    injectStorageFault: (position: "beforeAppend" | "afterAppend") => Promise<unknown>;
    /** The `.alto` bytes a save would write, without marking the document saved. */
    saveBytes: () => Promise<Uint8Array<ArrayBuffer>>;
    validateFrame: () => Promise<unknown>;
    stallDecodeWorkers: () => Promise<number>;
};
