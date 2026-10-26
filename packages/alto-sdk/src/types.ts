/** Data the engine and host exchange: document nodes, text, vectors, versions, assets. */

export type NodeId = string;
export type BlendMode =
  | "normal"
  | "multiply"
  | "screen"
  | "overlay"
  | "darken"
  | "lighten"
  | "color_dodge"
  | "color_burn"
  | "hard_light"
  | "soft_light"
  | "difference"
  | "exclusion"
  | "hue"
  | "saturation"
  | "color"
  | "luminosity"
  | "add";
/** Linear RGBA, each 0–1. */
export type Color = [number, number, number, number];
/** A 2D affine matrix `[a, b, c, d, e, f]`: x' = a·x + c·y + e, y' = b·x + d·y + f. */
export type Affine = [number, number, number, number, number, number];
export interface Rect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

/** Tiles of a raster, mask or selection, keyed `tx,ty`, valued by content hash. */
export interface TileRef {
  format: "rgba8" | "r8";
  tiles: Record<string, string>;
}

// ── Vectors ────────────────────────────────────────────────────────────────

export type PathCommand =
  | { op: "move" | "line"; x: number; y: number }
  | { op: "quad"; cx: number; cy: number; x: number; y: number }
  | { op: "cubic"; c1x: number; c1y: number; c2x: number; c2y: number; x: number; y: number }
  | { op: "close" };
export interface PathData {
  commands: PathCommand[];
}
export type FillRule = "non_zero" | "even_odd";
export interface VectorStroke {
  color: Color;
  width: number;
  cap?: "butt" | "round" | "square";
  join?: "miter" | "round" | "bevel";
  miter_limit?: number;
}
export interface StyledPath {
  data: PathData;
  fill: Color | null;
  stroke: VectorStroke | null;
  fill_rule?: FillRule;
}
export interface VectorMask {
  path: PathData;
  fill_rule?: FillRule;
  feather?: number;
  enabled?: boolean;
  transform?: Affine;
}
export interface ClipPath {
  path: PathData;
  fill_rule?: FillRule;
  enabled?: boolean;
  transform?: Affine;
}

// ── Text ───────────────────────────────────────────────────────────────────

export type WritingMode = "horizontal_tb" | "vertical_rl";
export interface FontFeature {
  tag: string;
  value: number;
}
export interface TextStyle {
  /** Fallback order: the first font that has every character of a grapheme is used. */
  fonts: string[];
  size: number;
  color?: Color;
  /** Line advance as a multiple of the size. */
  line_height?: number;
  /** Extra advance after each character, in em. */
  letter_spacing?: number;
  /** Horizontal glyph scale. */
  scale?: number;
  features?: FontFeature[];
  language?: string | null;
}
export type StylePatch = Partial<Omit<TextStyle, "line_height">>;
/** Offsets are UTF-16 code units, the same as JavaScript string indices. */
export interface TextSpan {
  start: number;
  end: number;
  style: StylePatch;
}
export interface TextRange {
  start: number;
  end: number;
}
export interface TextContent {
  text: string;
  style: TextStyle;
  spans?: TextSpan[];
  frame?: { kind: "point" } | { kind: "area"; width: number; height: number };
  writing_mode?: WritingMode;
  align?: "start" | "center" | "end" | "justify";
  /** Explicit tate-chu-yoko ranges (vertical text). */
  tcy?: TextRange[];
  /** Isolated runs of up to this many ASCII digits become tate-chu-yoko (0–4, default 2). */
  auto_tcy?: number;
}
export interface FontInfo {
  family: string;
  subfamily: string;
  face_index: number;
  faces_in_file: number;
  embedding: "installable" | "restricted" | "preview_and_print" | "editable";
  subsetting_allowed: boolean;
  units_per_em: number;
  glyph_count: number;
  has_vertical_metrics: boolean;
  has_vertical_alternates: boolean;
  outline: string;
}
export interface TextInfo {
  lines: { start: number; end: number; block: [number, number]; baseline: number }[];
  missing: number[];
  fonts_used: string[];
  glyphs: number;
  paths: number;
  bounds: [number, number, number, number];
  quad: [number, number][];
  length: number;
}
export interface TextCaret {
  offset: number;
  line: number;
  a: [number, number];
  b: [number, number];
}
export type TextMotion =
  | "forward"
  | "backward"
  | "line_start"
  | "line_end"
  | "next_line"
  | "previous_line"
  | "word_forward"
  | "word_backward"
  | "document_start"
  | "document_end";

// ── Effects ────────────────────────────────────────────────────────────────

/** Limits an effect to a selection mask; omitted or null applies it everywhere. */
export interface EffectScope {
  selection?: TileRef | null;
}
export type Effect =
  | ({ type: "gaussian_blur"; radius?: number } & EffectScope)
  | ({ type: "color_adjust"; brightness?: number; contrast?: number; saturation?: number } & EffectScope)
  | ({
      type: "levels";
      in_black?: number;
      in_white?: number;
      gamma?: number;
      out_black?: number;
      out_white?: number;
    } & EffectScope)
  | ({ type: "hue_saturation"; hue?: number; saturation?: number; lightness?: number } & EffectScope)
  | ({ type: "invert" } & EffectScope)
  | ({
      type: "curves";
      channel?: "master" | "red" | "green" | "blue";
      points: { x: number; y: number }[];
    } & EffectScope);

// ── Nodes ──────────────────────────────────────────────────────────────────

export interface NodeProps {
  visible: boolean;
  locked: boolean;
  opacity: number;
  blend: BlendMode;
  transform: Affine;
  mask: TileRef | null;
  mask_enabled: boolean;
  mask_linked: boolean;
  mask_transform: Affine;
  vector_mask: VectorMask | null;
  clip_path: ClipPath | null;
  clip_to_below: boolean;
  effects: Effect[];
}
/** Fields `Transaction.setProps` can change. */
export type PropPatch = Partial<
  Pick<
    NodeProps,
    | "visible"
    | "locked"
    | "opacity"
    | "blend"
    | "transform"
    | "clip_to_below"
    | "mask_enabled"
    | "mask_linked"
    | "mask_transform"
    | "effects"
  > & { name: string }
>;
export interface RasterTilesSummary {
  format: "rgba8" | "r8";
  count: number;
  /** Union of tile rectangles in layer pixels, `[x0, y0, x1, y1]`; null when empty. */
  bounds: [number, number, number, number] | null;
}
/** Fields every node has. */
export interface NodeBase {
  readonly id: NodeId;
  readonly parent: NodeId | null;
  readonly name: string;
  readonly props: Readonly<NodeProps>;
}
export interface GroupNode extends NodeBase {
  readonly type: "group";
  readonly children: readonly NodeId[];
  readonly isolation: "pass_through" | "isolated";
}
/** A raster layer's tile map itself comes from `DocumentHandle.nodeTiles`. */
export interface RasterNode extends NodeBase {
  readonly type: "raster";
  readonly tiles: RasterTilesSummary;
}
export interface TextNode extends NodeBase {
  readonly type: "text";
  readonly content: TextContent;
}
export interface VectorNode extends NodeBase {
  readonly type: "vector";
  readonly paths: readonly StyledPath[];
}
export interface AdjustmentNode extends NodeBase {
  readonly type: "adjustment";
  readonly adjustment: Effect;
}
/** A node kind this version does not know; the engine keeps it as it was. */
export interface UnsupportedNode extends NodeBase {
  readonly type: "unsupported";
  readonly raw: unknown;
}
export type DocNode = GroupNode | RasterNode | TextNode | VectorNode | AdjustmentNode | UnsupportedNode;

export interface DocumentMeta {
  readonly width: number;
  readonly height: number;
  readonly color: string;
  readonly depth: string;
  readonly format_version: number;
}

// ── Assets ─────────────────────────────────────────────────────────────────

export type AssetKind =
  | { kind: "encoded_image"; mime: string }
  | { kind: "font"; family: string; face_index: number; embedding: FontInfo["embedding"] }
  | { kind: "brush_tip" }
  | { kind: "pattern" };
/** An image or font the document holds. */
export interface DocumentAsset {
  readonly id: string;
  readonly hash: string;
  readonly kind: AssetKind;
  readonly byte_len: number;
  readonly dims: [number, number] | null;
}
declare const imported: unique symbol;
/** An imported image, ready for `Transaction.addRasterFromAsset`. Its record is the engine's. */
export interface ImageAsset {
  readonly [imported]: "image";
  /** The asset id the document will know it by. */
  readonly id: string;
  readonly width: number;
  readonly height: number;
}
/** An imported font, ready for `Transaction.addFont`. */
export interface FontAsset {
  readonly [imported]: "font";
  /** The asset id `TextStyle.fonts` names it by. */
  readonly id: string;
  readonly info: FontInfo;
  readonly warnings: readonly AltoWarning[];
}

// ── Versions ───────────────────────────────────────────────────────────────

export interface IntentSummary {
  intent: string;
  label: string | null;
  /** An undo of an edit an earlier version contained. */
  undone: boolean;
  count: number;
  targets: NodeId[];
  /** Null for a node that no longer exists. */
  target_names: (string | null)[];
}
export interface CommitView {
  hash: string;
  parents: string[];
  snapshot: string;
  message: string;
  created_at: number;
  author: string | null;
  intents: IntentSummary[];
}
/** What changed inside a node, for the host to word. */
export type ContentDetail =
  | { kind: "vector"; paths: [number, number]; commands: [number, number] }
  | {
      kind: "text";
      /** Excerpts before and after, when the text changed. */
      text: [string, string] | null;
      style: boolean;
      writing_mode: WritingMode | null;
      frame: boolean;
      tate_chu_yoko: boolean;
    }
  | { kind: "adjustment" }
  | { kind: "unsupported" }
  | { kind: "kind_changed"; from: string; to: string };
export type DiffItem =
  | { type: "added"; id: string; name: string; kind: string; parent: string }
  | { type: "removed"; id: string; name: string; kind: string }
  | { type: "moved"; id: string; name: string; from: [string, number]; to: [string, number] }
  | { type: "props_changed"; id: string; name: string; fields: string[] }
  | {
      type: "pixels_changed";
      id: string;
      name: string;
      target: "content" | "mask";
      tiles: number;
      bounds: Rect;
      document_bounds: Rect;
    }
  | { type: "content_changed"; id: string; name: string; kind: string; detail: ContentDetail }
  | { type: "asset_changed"; id: string; change: "added" | "removed" | "changed"; kind: string }
  | { type: "meta_changed"; fields: string[] }
  | { type: "selection_changed"; tiles: number };
export interface VersionDiff {
  items: DiffItem[];
  names: Record<string, string>;
}
export interface VersionStatus {
  head: string | null;
  commits: number;
  changed: boolean;
  changes: number;
  pending_intents: number;
}

// ── Warnings ───────────────────────────────────────────────────────────────

export type WarningCode =
  | "RECOVERED_OLDER_GENERATION"
  | "RECOVERED_TRUNCATED"
  | "RECOVERY_DIVERGED"
  | "MISSING_TILE"
  | "PACKAGE_DAMAGED"
  | "COMMIT_DAMAGED"
  | "COMMIT_SNAPSHOT_MISSING"
  | "HISTORY_OBJECT_MISSING"
  | "FONT_EMBEDDING_RESTRICTED"
  | "FONT_SUBSETTING_FORBIDDEN"
  | "FONT_MISSING";
/** A condition the document opened with. `details` holds hashes, font families and such. */
export interface AltoWarning {
  readonly code: WarningCode;
  readonly details: Readonly<Record<string, string>>;
}
