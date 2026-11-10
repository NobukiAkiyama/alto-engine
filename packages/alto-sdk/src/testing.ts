/**
 * Hooks for the engine's own tests: crashes, fault injection, damaged journals.
 * Not covered by the version promise. Product code must not use it.
 */
import type { Engine } from "./engine.js";
import { coreOf } from "./internal.js";

export function testing(engine: Engine) {
  const core = coreOf(engine);
  const request = core.request.bind(core);
  return {
    /** Any Worker request by name. */
    request: (t: string, payload?: Record<string, unknown>) => core.requestRaw(t, payload),
    /** Restarts the Worker as a crash would. */
    crash: () => core.restart(),
    destroyGpu: () => request("destroyGpu"),
    /** The module URL the next Worker start loads; null for the bundled one. */
    setWorkerURL: (url: URL | string | null) => {
      core.workerURL = url;
    },
    pendingRequests: () => (core as unknown as { pending: Map<number, unknown> }).pending.size,
    /** Generation, record count, object count and pack count of the recovery store. */
    recoveryStore: () =>
      request("recoveryStore"),
    corruptJournalTail: () => request("corruptJournalTail"),
    corruptCheckpoint: () => request("corruptCheckpoint"),
    corruptCheckpointCrc: () => request("corruptCheckpointCrc"),
    corruptBothCheckpoints: () => request("corruptBothCheckpoints"),
    /** Writes a format 1 checkpoint, as an older engine did. */
    legacyCheckpoint: () => request("legacyCheckpoint"),
    /** Writes a format 3 checkpoint (one file per object). */
    looseCheckpoint: () => request("looseCheckpoint"),
    injectStorageFault: (position: "beforeAppend" | "afterAppend") => request("injectStorageFault", { position }),
    /**
     * Stops journal and checkpoint writes before the next one, which stays in memory
     * until `holdStorage(false)`. Edits still apply; `durable()` waits.
     */
    partialAppend: (position: "header" | "payload" | "crc") => request("partialAppend", { position }),
    partialAppendHeld: () => request("partialAppendHeld"),
    holdStorage: (hold: boolean) => request("holdStorage", { hold }),
    /**
     * Ends the next checkpoint after its pack is flushed ("pack"), after a torn journal
     * write ("journal"), or after the whole journal is flushed ("flushed"), and never
     * reports it done. Storage must be held first.
     */
    stopCheckpoint: (stage: "pack" | "journal" | "flushed") => request("stopCheckpoint", { stage }),
    /** Resolves once the armed checkpoint stop has been reached. */
    checkpointStopped: () => request("checkpointStopped"),
    /** Resolves once a write waits at the hold, with its kind. */
    storageHeld: () => request("storageHeld"),
    /** The `.alto` bytes a save would write, without marking the document saved. */
    saveBytes: async () => {
      await core.document?.endTool();
      return new Uint8Array((await request("save")).bytes);
    },
    validateFrame: () => request("validateFrame"),
    stallDecodeWorkers: () => request("stallDecodeWorkers"),
  };
}
