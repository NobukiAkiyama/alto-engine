/**
 * Diagnostics: per-frame statistics, render pipeline switches and tile readback.
 * Not covered by the version promise; any release may change it.
 */
import type { Engine } from "./engine.js";
import type { PathData } from "./types.js";
/** Numbers the Worker reports after each frame (names vary by build). */
export type FrameStats = Record<string, unknown>;
export declare function unstable(engine: Engine): {
    /** Resolves when the current view has finished drawing. */
    settle: () => Promise<FrameStats>;
    readTile: (x: number, y: number) => Promise<Uint8Array<ArrayBuffer>>;
    readTileLevel: (level: number, x: number, y: number) => Promise<Uint8Array<ArrayBuffer>>;
    vectorStats: () => Promise<Record<string, number>>;
    /** Small text through the glyph atlas (default) or always through the vector path. */
    setGlyphAtlas: (enabled: boolean) => Promise<unknown>;
    setVisibleTileJobs: (enabled: boolean) => Promise<unknown>;
    /** Resumes prefetching after `resetRenderCache` paused it. */
    enablePrefetch: () => Promise<unknown>;
    /** Empties the display caches (and the CPU tile cache with `cpu`); pauses prefetching. */
    resetRenderCache: (cpu: boolean) => Promise<unknown>;
    setProfiling: (enabled: boolean) => Promise<unknown>;
    /** Resolves to whether decode Workers are running afterwards. */
    setDecodeWorkers: (enabled: boolean) => Promise<boolean>;
    /** The device pixel ratio the canvas renders at. */
    pixelRatio: () => number;
    onFrame: (listener: (stats: FrameStats) => void) => () => void;
    onDiagnostic: (listener: (detail: Record<string, unknown>) => void) => () => void;
    onGpu: (listener: (event: "lost" | "restored") => void) => () => void;
    parseVectorPath: (svg: string) => Promise<PathData>;
};
