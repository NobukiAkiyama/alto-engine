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
  const request = (t: string, payload: Record<string, unknown> = {}) => core.request(t, payload);
  return {
    /** Resolves when the current view has finished drawing. */
    settle: () => request("waitFrame") as Promise<FrameStats>,
    readTile: async (x: number, y: number) => new Uint8Array((await request("readTile", { x, y })) as ArrayBuffer),
    readTileLevel: async (level: number, x: number, y: number) =>
      new Uint8Array((await request("readTileLevel", { level, x, y })) as ArrayBuffer),
    vectorStats: () => request("vectorStats") as Promise<Record<string, number>>,
    /** Small text through the glyph atlas (default) or always through the vector path. */
    setGlyphAtlas: (enabled: boolean) => request("setGlyphAtlas", { enabled }),
    setVisibleTileJobs: (enabled: boolean) => request("setVisibleTileJobs", { enabled }),
    /** Resumes prefetching after `resetRenderCache` paused it. */
    enablePrefetch: () => request("enablePrefetch"),
    /** Empties the display caches (and the CPU tile cache with `cpu`); pauses prefetching. */
    resetRenderCache: (cpu: boolean) => request("resetRenderCache", { cpu }),
    setProfiling: (enabled: boolean) => request("setProfiling", { enabled }),
    /** Resolves to whether decode Workers are running afterwards. */
    setDecodeWorkers: (enabled: boolean) => request("setDecodeWorkers", { enabled }) as Promise<boolean>,
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
    parseVectorPath: (svg: string) => request("parseVectorPath", { svg }) as Promise<PathData>,
  };
}
