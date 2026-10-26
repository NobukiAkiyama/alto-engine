/**
 * Diagnostics: per-frame statistics, render pipeline switches and tile readback.
 * Not covered by the version promise; any release may change it.
 */
import type { Engine } from "./engine.js";
import { coreOf } from "./internal.js";
import type { PathData } from "./types.js";

/** Numbers the Worker reports after each frame (names vary by build). */
export type FrameStats = Record<string, unknown>;

export function unstable(engine: Engine) {
  const core = coreOf(engine);
  return {
    /** Resolves when the current view has finished drawing. */
    settle: () => core.request("waitFrame"),
    readTile: (x: number, y: number) => core.request("readTile", { x, y }),
    readTileLevel: (level: number, x: number, y: number) => core.request("readTileLevel", { level, x, y }),
    vectorStats: () => core.request("vectorStats"),
    /** Small text through the glyph atlas (default) or always through the vector path. */
    setGlyphAtlas: (enabled: boolean) => core.request("setGlyphAtlas", { enabled }),
    setVisibleTileJobs: (enabled: boolean) => core.request("setVisibleTileJobs", { enabled }),
    /** Resumes prefetching after `resetRenderCache` paused it. */
    enablePrefetch: () => core.request("enablePrefetch"),
    /** Empties the display caches (and the CPU tile cache with `cpu`); pauses prefetching. */
    resetRenderCache: (cpu: boolean) => core.request("resetRenderCache", { cpu }),
    setProfiling: (enabled: boolean) => core.request("setProfiling", { enabled }),
    /** Resolves to whether decode Workers are running afterwards. */
    setDecodeWorkers: (enabled: boolean) => core.request("setDecodeWorkers", { enabled }),
    /** The device pixel ratio the canvas renders at. */
    pixelRatio: () => core.pixelRatioValue,
    onFrame: (listener: (stats: FrameStats) => void) => core.diagnostics.on("frame", listener),
    onDiagnostic: (listener: (detail: Record<string, unknown>) => void) => {
      const a = core.diagnostics.on("diagnostic", listener);
      const b = core.diagnostics.on("workerDiagnostic", listener);
      return () => {
        a();
        b();
      };
    },
    onGpu: (listener: (event: "lost" | "restored") => void) => {
      const a = core.diagnostics.on("gpuLost", () => listener("lost"));
      const b = core.diagnostics.on("gpuRestored", () => listener("restored"));
      return () => {
        a();
        b();
      };
    },
    parseVectorPath: (svg: string) => core.request("parseVectorPath", { svg }),
  };
}
