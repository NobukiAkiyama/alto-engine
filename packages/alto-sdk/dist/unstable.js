import { coreOf } from "./internal.js";
export function unstable(engine) {
    const core = coreOf(engine);
    const request = (t, payload = {}) => core.request(t, payload);
    return {
        /** Resolves when the current view has finished drawing. */
        settle: () => request("waitFrame"),
        readTile: async (x, y) => new Uint8Array((await request("readTile", { x, y }))),
        readTileLevel: async (level, x, y) => new Uint8Array((await request("readTileLevel", { level, x, y }))),
        vectorStats: () => request("vectorStats"),
        /** Small text through the glyph atlas (default) or always through the vector path. */
        setGlyphAtlas: (enabled) => request("setGlyphAtlas", { enabled }),
        setVisibleTileJobs: (enabled) => request("setVisibleTileJobs", { enabled }),
        /** Resumes prefetching after `resetRenderCache` paused it. */
        enablePrefetch: () => request("enablePrefetch"),
        /** Empties the display caches (and the CPU tile cache with `cpu`); pauses prefetching. */
        resetRenderCache: (cpu) => request("resetRenderCache", { cpu }),
        setProfiling: (enabled) => request("setProfiling", { enabled }),
        /** Resolves to whether decode Workers are running afterwards. */
        setDecodeWorkers: (enabled) => request("setDecodeWorkers", { enabled }),
        /** The device pixel ratio the canvas renders at. */
        pixelRatio: () => core.pixelRatioValue,
        onFrame: (listener) => core.diagnostics.on("frame", listener),
        onDiagnostic: (listener) => {
            const a = core.diagnostics.on("diagnostic", listener);
            const b = core.diagnostics.on("workerDiagnostic", listener);
            return () => {
                a();
                b();
            };
        },
        onGpu: (listener) => {
            const a = core.diagnostics.on("gpuLost", () => listener("lost"));
            const b = core.diagnostics.on("gpuRestored", () => listener("restored"));
            return () => {
                a();
                b();
            };
        },
        parseVectorPath: (svg) => request("parseVectorPath", { svg }),
    };
}
