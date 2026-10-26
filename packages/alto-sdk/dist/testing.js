import { coreOf } from "./internal.js";
export function testing(engine) {
    const core = coreOf(engine);
    const request = core.request.bind(core);
    return {
        /** Any Worker request by name. */
        request: (t, payload) => core.requestRaw(t, payload),
        /** Restarts the Worker as a crash would. */
        crash: () => core.restart(),
        destroyGpu: () => request("destroyGpu"),
        /** The module URL the next Worker start loads; null for the bundled one. */
        setWorkerURL: (url) => {
            core.workerURL = url;
        },
        pendingRequests: () => core.pending.size,
        /** Generation, record count, object count and pack count of the recovery store. */
        recoveryStore: () => request("recoveryStore"),
        corruptJournalTail: () => request("corruptJournalTail"),
        corruptCheckpoint: () => request("corruptCheckpoint"),
        corruptCheckpointCrc: () => request("corruptCheckpointCrc"),
        corruptBothCheckpoints: () => request("corruptBothCheckpoints"),
        /** Writes a format 1 checkpoint, as an older engine did. */
        legacyCheckpoint: () => request("legacyCheckpoint"),
        /** Writes a format 3 checkpoint (one file per object). */
        looseCheckpoint: () => request("looseCheckpoint"),
        injectStorageFault: (position) => request("injectStorageFault", { position }),
        /** The `.alto` bytes a save would write, without marking the document saved. */
        saveBytes: async () => {
            await core.document?.endTool();
            return new Uint8Array((await request("save")).bytes);
        },
        validateFrame: () => request("validateFrame"),
        stallDecodeWorkers: () => request("stallDecodeWorkers"),
    };
}
