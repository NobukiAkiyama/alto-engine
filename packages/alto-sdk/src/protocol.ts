/**
 * @internal The messages between the SDK and its engine Worker, written once. The SDK's
 * `request` and the Worker's `execute` are both checked against `Requests`. Not part
 * of the public API: the SDK and Worker ship together.
 */
import type { WireState } from "./snapshot.js";
import type { ImportedImage, WireOp } from "./transaction.js";
import type {
  CommitView,
  FontInfo,
  PathData,
  Rect,
  TextCaret,
  TextInfo,
  TextMotion,
  TileRef,
  VersionDiff,
} from "./types.js";

export const PROTOCOL = 5;

/** Codes the Worker reports; the SDK maps them to `AltoError`. */
export interface WireError {
  code: string;
  message: string;
}
export interface RecoveryInfo {
  available: boolean;
  records: number;
  latestGeneration: number;
  truncated?: boolean;
}
export interface BrushConfig {
  size: number;
  hardness: number;
  spacing: number;
  opacity: number;
  flow: number;
  pressure_size: boolean;
  pressure_opacity: boolean;
}
export interface ImportedFontRecord {
  record: { id: string; hash: string; [field: string]: unknown };
  info: FontInfo;
  warnings: { code: string; details: Record<string, string> }[];
}
export type FrameStats = Record<string, unknown> & {
  /** Engine-managed allocation estimates, not browser/driver VRAM. */
  gpuBytes: number;
  gpu: {
    fixed: number;
    liveFixed: number;
    effects: number;
    stroke: number;
    vector: number;
    readback: number;
    reserved: number;
    pendingAllocation: number;
    budgetUsed: number;
    budgetLimit: number;
    operationPeak: number;
    retiredFixed: number;
    retired: number;
    effectsAllocations: number;
    effectsReuses: number;
    atlasLayers: number;
    displayLayers: number;
    overviewTiles: number;
    overviewReady: number;
  };
  display: Record<string, number>;
  documentEpoch?: number;
  /** Display tiles composited this frame. */
  composed: number;
  pending: number;
  /** 1 when this frame was held back until the previous one finishes on the GPU. */
  presentDeferred: number;
  surface: [number, number];
  viewVersion?: number;
  profile: Record<string, number> | null;
};
type None = Record<string, never>;
type Q<A, R> = { args: A; result: R };

/** Every request: its arguments and what the reply carries. */
export interface Requests {
  init: Q<
    { protocol: number; sdk: string; canvas: OffscreenCanvas; budget: number; namespace: string; journal: boolean },
    { protocol: number; recovery: RecoveryInfo }
  >;
  createDocument: Q<{ width: number; height: number }, WireState>;
  transact: Q<{ intent: string; label?: string; ops: WireOp[]; baseRevision: number }, WireState>;
  undo: Q<None, WireState>;
  redo: Q<None, WireState>;
  upgradeRenderVersion: Q<None, WireState>;
  beginBrush: Q<{ layer: string; config: BrushConfig; color: number[]; erase: boolean; mask: boolean }, true>;
  endBrush: Q<None, WireState>;
  cancelBrush: Q<None, true>;
  select: Q<{ spec: unknown }, WireState>;
  setMask: Q<{ layer: string; source: "selection" | "white" | "remove" }, WireState>;
  setEffects: Q<{ layer: string; effects: unknown[]; useSelection: boolean }, WireState>;
  selectionOutline: Q<None, string>;
  import: Q<{ width: number; height: number; pixels: ArrayBuffer; encoded: ArrayBuffer; mime: string }, ImportedImage>;
  emergencyExport: Q<None, Uint8Array>;
  save: Q<None, { bytes: Uint8Array; token: number }>;
  folderEntries: Q<None, { bytes: Uint8Array; token: number }>;
  folderPlan: Q<{ bytes: ArrayBuffer }, string[]>;
  openFolder: Q<{ bytes: ArrayBuffer }, WireState>;
  importFont: Q<{ bytes: ArrayBuffer; faceIndex: number }, ImportedFontRecord>;
  releaseImport: Q<{ hash: string }, boolean>;
  pruneVersions: Q<None, WireState & { removed?: number }>;
  snapshotHash: Q<None, string>;
  nodeTiles: Q<{ node: string }, TileRef>;
  textInfo: Q<{ node: string }, TextInfo>;
  textHitTest: Q<{ node: string; x: number; y: number }, number>;
  textCaret: Q<{ node: string; offset: number }, TextCaret | null>;
  textSelection: Q<{ node: string; start: number; end: number }, [number, number][][]>;
  textNavigate: Q<{ node: string; offset: number; motion: TextMotion }, number>;
  commit: Q<{ message: string; author: string; allowEmpty: boolean; time: number }, { commit: CommitView; state: WireState }>;
  versionLog: Q<None, CommitView[]>;
  versionDiff: Q<{ from: string; to: string }, VersionDiff>;
  restoreVersion: Q<{ hash: string }, WireState>;
  saved: Q<{ token: number }, WireState>;
  releaseSave: Q<{ token: number }, boolean>;
  open: Q<{ bytes: ArrayBuffer }, WireState>;
  recover: Q<None, WireState>;
  discardRecovery: Q<None, true>;
  /** Answered beside the queue, once the journal holds every edit so far. */
  durable: Q<None, true>;
  view: Q<None, WireState>;
  parseVectorPath: Q<{ svg: string }, PathData>;
  vectorPathSvg: Q<{ data: PathData }, string>;
  exportImage: Q<{ region: Rect; scale: number; type: string }, Blob>;
  injectFinishJournalFailure: Q<None, true>;
  injectReadbackFailure: Q<{ after: number }, true>;
  // Diagnostics (unstable entry point).
  waitFrame: Q<None, FrameStats>;
  readTile: Q<{ x: number; y: number }, Uint8Array>;
  /** `fresh` empties the render caches first, in the same Worker step. */
  readTileLevel: Q<{ level: number; x: number; y: number; fresh?: boolean; pyramid?: boolean }, Uint8Array>;
  vectorStats: Q<None, Record<string, number>>;
  setGlyphAtlas: Q<{ enabled: boolean }, true>;
  setVisibleTileJobs: Q<{ enabled: boolean }, true>;
  resetRenderCache: Q<{ cpu: boolean }, true>;
  enablePrefetch: Q<None, true>;
  setProfiling: Q<{ enabled: boolean }, true>;
  setDecodeWorkers: Q<{ enabled: boolean }, boolean>;
  // Test hooks (testing entry point).
  validateFrame: Q<None, true>;
  cpuReference: Q<{ config: BrushConfig; color: number[]; samples: number[] }, Uint8Array>;
  stallDecodeWorkers: Q<None, number>;
  destroyGpu: Q<None, true>;
  /** Test hook: the next ticks look like an idle deferred present, until `limit` of them. */
  stickPresent: Q<{ enabled: boolean; limit?: number }, true>;
  recoveryStore: Q<None, { latestGeneration: number; records: number; objects: number; packs: number }>;
  corruptJournalTail: Q<None, unknown>;
  corruptCheckpoint: Q<None, unknown>;
  corruptCheckpointCrc: Q<None, unknown>;
  corruptBothCheckpoints: Q<None, unknown>;
  looseCheckpoint: Q<None, true>;
  legacyCheckpoint: Q<None, true>;
  injectStorageFault: Q<{ position: "beforeAppend" | "afterAppend" }, true>;
}
export type RequestName = keyof Requests;
export type Args<K extends RequestName> = Requests[K]["args"];
export type Result<K extends RequestName> = Requests[K]["result"];
/** Ownership of the document a request reads or changes (SDK–Worker only). */
export interface DocumentScope {
  documentEpoch?: number;
  nextDocumentEpoch?: number;
}
/** A request as posted: `t` names it, `id` pairs the reply. */
export type RequestMessage = { [K in RequestName]: Args<K> & { t: K; id: number } }[RequestName] & DocumentScope;

/** Fire-and-forget messages: frames, input samples and view changes. */
export type StreamMessage =
  | { t: "tick"; time: number }
  | { t: "viewport"; version: number; center: [number, number]; zoom: number; rotation: number }
  | { t: "resize"; width: number; height: number }
  | { t: "input"; samples: Float32Array; documentEpoch?: number };

export type Reply = { t: "reply"; id: number } & ({ ok: true; value: unknown } | { ok: false; error: WireError });

/** What the Worker posts without being asked. */
export type Notice =
  | { t: "patch"; value: WireState }
  | { t: "frame"; value: FrameStats }
  | { t: "storageFailure"; value: WireError }
  | { t: "diagnostic"; value: Record<string, unknown> }
  | { t: "diagnostic-storage"; value: string }
  | { t: "gpuLost" }
  | { t: "gpuRestored" }
  | { t: "error"; value: WireError };

export type WorkerMessage = Reply | Notice;
