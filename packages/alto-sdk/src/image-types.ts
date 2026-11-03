import type { NodeId } from "./types.js";
/** @internal What an operation is about to read from a folder. */
export type ObjectTarget =
  | { type: "commit"; hash: string }
  | { type: "branch"; name: string }
  | { type: "merge"; name: string }
  | { type: "merge_plan" }
  | { type: "all" };
/** Exact integer extent in node-local pixels; transparent images retain this extent. */
export interface ImageBounds { x: number; y: number; width: number; height: number }
/** Straight sRGB channels, each between zero and one. */
export type ImageColor = [number, number, number, number];
/** `catmull_rom` is sharper when enlarging. `area` averages the source under each pixel, for large reductions. */
export type ImageFilter = "nearest" | "bilinear" | "catmull_rom" | "area";
export type ImageSelection = "current" | "ignore";
export interface GradientOptions {
  kind: "linear" | "radial";
  start: [number, number]; end: [number, number];
  stops: { offset: number; color: ImageColor }[];
  opacity?: number; selection?: ImageSelection;
}
export interface ImageEditResult { node: NodeId; bounds: ImageBounds; changed_tiles: number; no_op: boolean }
/** Result of a whole-document edit. Layer pixels are not rewritten. */
export interface CanvasEditResult { width: number; height: number; no_op: boolean }
export interface LayerEditResult { node: NodeId; bounds: ImageBounds; no_op: boolean }
/** Stored pixels per channel, before opacity, masks and effects. Each channel has 256 bins. */
export interface ImageHistogram { r: number[]; g: number[]; b: number[]; a: number[]; pixels: number }
/** A polyline in node-local pixels, drawn with a round tip. */
export interface ImageStroke { points: [number, number][]; radius: number; hardness?: number }
export interface PickedColor { bytes: [number, number, number, number]; color: ImageColor }
export interface BranchView { name: string; head: string; current: boolean; protected: boolean; ahead: number; behind: number }
export interface VersionRefs {
  current_branch: string | null;
  branches: Record<string, { head: string; protected: boolean }>;
  tags: Record<string, string>; pinned: string[]; generation: number; gc_generation: number;
}
export interface MergeValue { present: boolean; value: unknown }
export interface MergeConflict { id: string; kind: string; path: string[]; base: MergeValue; ours: MergeValue; theirs: MergeValue }
/** Every plan, including a no-op or fast-forward, is finalized using its token. */
export interface MergePlan { kind: "no_op" | "fast_forward" | "three_way"; token: string; ours: string; theirs: string; base: string | null; conflicts: MergeConflict[] }
export interface MergeResolution { conflict_id: string; choice: "ours" | "theirs" | "explicit"; value?: MergeValue }
export interface MergeResult { kind: "no_op" | "fast_forward" | "committed"; head: string; base: string | null }

export interface SvgImportResult { node: NodeId; nodes: number }

export interface GcCounts { kept: number; removed: number; kept_bytes: number; removed_bytes: number }
export interface GcReport { warnings: string[]; dry_run: boolean; complete: boolean; baseline: string; roots: number; by_kind: Record<string, GcCounts> }
export interface GcOptions { dryRun?: boolean; expectedBaseline?: string }
