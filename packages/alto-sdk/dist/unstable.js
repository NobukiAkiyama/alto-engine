import { coreOf } from "./internal.js";
export function unstable(engine) {
    const core = coreOf(engine);
    return {
        /** Resolves when the current view has finished drawing. */
        settle: () => core.request("waitFrame"),
        readTile: (x, y) => core.request("readTile", { x, y }),
        readTileLevel: (level, x, y) => core.request("readTileLevel", { level, x, y }),
        vectorStats: () => core.request("vectorStats"),
        /** Small text through the glyph atlas (default) or always through the vector path. */
        setGlyphAtlas: (enabled) => core.request("setGlyphAtlas", { enabled }),
        setVisibleTileJobs: (enabled) => core.request("setVisibleTileJobs", { enabled }),
        /** Resumes prefetching after `resetRenderCache` paused it. */
        enablePrefetch: () => core.request("enablePrefetch"),
        /** Empties the display caches (and the CPU tile cache with `cpu`); pauses prefetching. */
        resetRenderCache: (cpu) => core.request("resetRenderCache", { cpu }),
        setProfiling: (enabled) => core.request("setProfiling", { enabled }),
        /** Resolves to whether decode Workers are running afterwards. */
        setDecodeWorkers: (enabled) => core.request("setDecodeWorkers", { enabled }),
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
        parseVectorPath: (svg) => core.request("parseVectorPath", { svg }),
    };
}
