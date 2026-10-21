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
export type Color = [number,number,number,number];
export type PathCommand = {op:'move'|'line';x:number;y:number} | {op:'quad';cx:number;cy:number;x:number;y:number} | {op:'cubic';c1x:number;c1y:number;c2x:number;c2y:number;x:number;y:number} | {op:'close'};
export interface PathData {commands:PathCommand[]}
export type FillRule = 'non_zero'|'even_odd';
export interface VectorStroke {color:Color;width:number;cap?:'butt'|'round'|'square';join?:'miter'|'round'|'bevel';miter_limit?:number}
export interface StyledPath {data:PathData;fill:Color|null;stroke:VectorStroke|null;fill_rule?:FillRule}
export interface VectorMask {path:PathData;fill_rule?:FillRule;feather?:number;enabled?:boolean;transform?:number[]}
export interface ClipPath {path:PathData;fill_rule?:FillRule;enabled?:boolean;transform?:number[]}
export type WritingMode = "horizontal_tb" | "vertical_rl";
export interface FontFeature { tag: string; value: number }
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
export interface TextSpan { start: number; end: number; style: StylePatch }
export interface TextRange { start: number; end: number }
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
  family: string; subfamily: string; face_index: number; faces_in_file: number;
  embedding: "installable" | "restricted" | "preview_and_print" | "editable";
  subsetting_allowed: boolean; units_per_em: number; glyph_count: number;
  has_vertical_metrics: boolean; has_vertical_alternates: boolean; outline: string;
}
export interface FontAsset { record: any; info: FontInfo; warnings: string[] }
export interface TextInfo {
  lines: { start: number; end: number; block: [number, number]; baseline: number }[];
  missing: number[]; fonts_used: string[]; glyphs: number; paths: number;
  bounds: [number, number, number, number]; quad: [number, number][]; length: number;
}
export interface TextCaret { offset: number; line: number; a: [number, number]; b: [number, number] }
export type TextMotion = "forward" | "backward" | "line_start" | "line_end" | "next_line" | "previous_line" | "word_forward" | "word_backward" | "document_start" | "document_end";
export interface CommitView {
  hash: string; parents: string[]; snapshot: string; message: string; created_at: number; author: string | null;
  intents: { intent: string; count: number; targets: string[]; target_names: string[] }[];
}
export type DiffItem =
  | { type: "added"; id: string; name: string; kind: string; parent: string }
  | { type: "removed"; id: string; name: string; kind: string }
  | { type: "moved"; id: string; name: string; from: [string, number]; to: [string, number] }
  | { type: "props_changed"; id: string; name: string; fields: string[] }
  | { type: "pixels_changed"; id: string; name: string; target: "content" | "mask"; tiles: number; bounds: Rect; document_bounds: Rect }
  | { type: "content_changed"; id: string; name: string; kind: string; detail: string }
  | { type: "asset_changed"; id: string; change: "added" | "removed" | "changed"; kind: string }
  | { type: "meta_changed"; fields: string[] }
  | { type: "selection_changed"; tiles: number };
export interface Rect { x0: number; y0: number; x1: number; y1: number }
export interface VersionDiff { items: DiffItem[]; names: Record<string, string> }
export interface VersionStatus { head: string | null; commits: number; changed: boolean; changes: number; pending_intents: number }
export interface NodeView {
  id: NodeId;
  parent: NodeId | null;
  name: string;
  props: {
    visible: boolean;
    locked: boolean;
    opacity: number;
    blend: string;
    transform: number[];
    mask: { format: string; tiles: Record<string, string> } | null;
    mask_enabled: boolean;
    mask_linked: boolean;
    mask_transform: number[];
    vector_mask: VectorMask|null;
    clip_path: ClipPath|null;
    clip_to_below: boolean;
    effects: unknown[];
  };
  kind: {
    kind: string;
    children?: NodeId[];
    tiles?: { format: string; tiles: Record<string, string> };
    [key: string]: unknown;
  };
  children: NodeView[];
}
export interface Mirror {
  meta: {
    width: number;
    height: number;
    color: string;
    depth: string;
    format_version: number;
  };
  root: string;
  nodes: Record<string, NodeView>;
  revision: number;
  selection: { format: string; tiles: Record<string, string> } | null;
}
export interface AssetRef {
  record: any;
  tiles: { format: string; tiles: Record<string, string> };
  width: number;
  height: number;
}
export interface BrushOptions {
  layer: NodeId;
  target?: "content" | "mask";
  tip?: { size?: number; hardness?: number; spacing?: number };
  color?: number[];
  opacity?: number;
  flow?: number;
  pressureSize?: boolean;
  pressureOpacity?: boolean;
}
export interface ArchiveStorageProvider {
  readonly kind: string;
  readArchive(): Promise<Blob>;
  writeArchive(stream: ReadableStream<Uint8Array>): Promise<void>;
}
/**
 * Object-level storage for the folder form (`.altoproj/`). Content-addressed files
 * (objects/, snapshots/, commits/) are addressed by package path and never rewritten.
 * Refs (manifest.json and the readable mirrors) are replaced; `expectHash` is the
 * SHA-256 hex of the ref the caller based its write on (optimistic lock, "" = absent).
 */
export interface ObjectStorageProvider {
  readonly kind: string;
  getObject(path: string): Promise<Uint8Array | null>;
  /** Idempotent: existing paths are skipped. */
  putObjects(entries: { path: string; data: Uint8Array }[]): Promise<{ written: number; skipped: number }>;
  list(prefix: string): Promise<string[]>;
  readRef(name: string): Promise<Uint8Array | null>;
  writeRef(name: string, data: Uint8Array, expectHash?: string): Promise<void>;
}
export async function sha256Hex(data: Uint8Array): Promise<string> {
  const d = await crypto.subtle.digest("SHA-256", data as BufferSource);
  return Array.from(new Uint8Array(d), (b) => b.toString(16).padStart(2, "0")).join("");
}
const immutable = (p: string) => p.startsWith("objects/") || p.startsWith("snapshots/") || p.startsWith("commits/");
function validPath(p: string) {
  return !!p && !p.startsWith("/") && p.split("/").every((s) => s && s !== "." && s !== "..");
}
export class MemoryObjectProvider implements ObjectStorageProvider {
  readonly kind = "memory";
  readonly files = new Map<string, Uint8Array>();
  async getObject(path: string) { return this.files.get(path)?.slice() ?? null; }
  async putObjects(entries: { path: string; data: Uint8Array }[]) {
    let written = 0, skipped = 0;
    for (const e of entries) {
      if (!validPath(e.path)) throw new AltoError("INVALID_ARGUMENT", "不正なパス: " + e.path);
      if (this.files.has(e.path)) skipped++;
      else { this.files.set(e.path, e.data.slice()); written++; }
    }
    return { written, skipped };
  }
  async list(prefix: string) { return [...this.files.keys()].filter((k) => k.startsWith(prefix)).sort(); }
  async readRef(name: string) { return this.getObject(name); }
  async writeRef(name: string, data: Uint8Array, expectHash?: string) {
    if (expectHash !== undefined) {
      const cur = this.files.get(name);
      if ((cur ? await sha256Hex(cur) : "") !== expectHash) throw new AltoError("CONFLICT", name + " が他で変更されています");
    }
    this.files.set(name, data.slice());
  }
}
/** A directory from the File System Access API or OPFS. Writes replace atomically on close. */
export class FileSystemDirectoryProvider implements ObjectStorageProvider {
  readonly kind = "fs-access-dir";
  constructor(readonly handle: FileSystemDirectoryHandle) {}
  private async dir(parts: string[], create: boolean) {
    let d = this.handle;
    for (const p of parts) d = await d.getDirectoryHandle(p, { create });
    return d;
  }
  private async file(path: string, create: boolean) {
    if (!validPath(path)) throw new AltoError("INVALID_ARGUMENT", "不正なパス: " + path);
    const parts = path.split("/");
    const name = parts.pop()!;
    return (await this.dir(parts, create)).getFileHandle(name, { create });
  }
  async getObject(path: string) {
    try {
      const f = await (await this.file(path, false)).getFile();
      return new Uint8Array(await f.arrayBuffer());
    } catch (e: any) {
      if (e?.name === "NotFoundError" || e?.name === "TypeError") return null;
      throw new AltoError("STORAGE", String(e));
    }
  }
  private async write(path: string, data: Uint8Array) {
    try {
      const w = await (await this.file(path, true)).createWritable();
      try { await w.write(data as BufferSource); await w.close(); } catch (e) { await w.abort(); throw e; }
    } catch (e: any) {
      throw e instanceof AltoError ? e : new AltoError("STORAGE", String(e));
    }
  }
  async putObjects(entries: { path: string; data: Uint8Array }[]) {
    let written = 0, skipped = 0;
    for (const e of entries) {
      if (await this.exists(e.path)) skipped++;
      else { await this.write(e.path, e.data); written++; }
    }
    return { written, skipped };
  }
  private async exists(path: string) {
    try { await this.file(path, false); return true; } catch { return false; }
  }
  async list(prefix: string) {
    const out: string[] = [];
    const parts = prefix.split("/").filter(Boolean);
    let root: FileSystemDirectoryHandle;
    try { root = await this.dir(parts, false); } catch { return out; }
    const walk = async (d: FileSystemDirectoryHandle, base: string) => {
      for await (const [name, h] of (d as any).entries() as AsyncIterable<[string, FileSystemHandle]>) {
        if (name.startsWith(".")) continue;
        if (h.kind === "directory") await walk(h as FileSystemDirectoryHandle, base + name + "/");
        else out.push(base + name);
      }
    };
    await walk(root, parts.length ? parts.join("/") + "/" : "");
    return out.sort();
  }
  async readRef(name: string) { return this.getObject(name); }
  async writeRef(name: string, data: Uint8Array, expectHash?: string) {
    if (expectHash !== undefined) {
      const cur = await this.getObject(name);
      if ((cur ? await sha256Hex(cur) : "") !== expectHash) throw new AltoError("CONFLICT", name + " が他で変更されています");
    }
    await this.write(name, data);
  }
}
/** Worker <-> main framing for package entries. */
export function frameEntries(entries: Map<string, Uint8Array>): Uint8Array {
  const enc = new TextEncoder();
  const names = [...entries.keys()].map((k) => enc.encode(k));
  let size = 4;
  let i = 0;
  for (const v of entries.values()) size += 8 + names[i++].length + v.length;
  const out = new Uint8Array(size);
  const view = new DataView(out.buffer);
  view.setUint32(0, entries.size, true);
  let p = 4;
  i = 0;
  for (const v of entries.values()) {
    const n = names[i++];
    view.setUint32(p, n.length, true); out.set(n, p + 4); p += 4 + n.length;
    view.setUint32(p, v.length, true); out.set(v, p + 4); p += 4 + v.length;
  }
  return out;
}
export function unframeEntries(b: Uint8Array): Map<string, Uint8Array> {
  const view = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const dec = new TextDecoder();
  const out = new Map<string, Uint8Array>();
  let p = 4;
  for (let i = 0, n = view.getUint32(0, true); i < n; i++) {
    const l = view.getUint32(p, true);
    const name = dec.decode(b.subarray(p + 4, p + 4 + l)); p += 4 + l;
    const d = view.getUint32(p, true);
    out.set(name, b.subarray(p + 4, p + 4 + d)); p += 4 + d;
  }
  return out;
}
/** Replaces a UTF-16 range; spans shift (typing at a span's end extends it), edits inside tate-chu-yoko dissolve it. */
export function replaceText(content: TextContent, start: number, end: number, insert: string): TextContent {
  const c = structuredClone(content);
  if (start > end) [start, end] = [end, start];
  const added = insert.length, removed = end - start;
  // Text inserted at a boundary joins the span ending there, so typing continues the preceding style.
  const shift = (p: number) => (p < start ? p : p >= end ? p - removed + added : start + added);
  c.text = c.text.slice(0, start) + insert + c.text.slice(end);
  // Typing over a selection keeps the style of the replaced text.
  c.spans = (c.spans || []).map((s) => ({ ...s, start: s.start === start && removed > 0 ? s.start : shift(s.start), end: s.end === start ? s.end + added : shift(s.end) })).filter((s) => s.start < s.end);
  c.tcy = (c.tcy || [])
    .filter((r) => !(start < r.end && end > r.start) && !(start === end && start > r.start && start < r.end))
    .map((r) => ({ start: shift(r.start), end: shift(r.end) }));
  return c;
}
/** Applies a style patch to a UTF-16 range, splitting and merging spans. */
export function applyTextStyle(content: TextContent, start: number, end: number, patch: StylePatch): TextContent {
  const c = structuredClone(content);
  if (start > end) [start, end] = [end, start];
  const spans = c.spans || [];
  const cuts = [...new Set([0, c.text.length, start, end, ...spans.flatMap((s) => [s.start, s.end])])].filter((x) => x >= 0 && x <= c.text.length).sort((a, b) => a - b);
  const out: TextSpan[] = [];
  for (let i = 0; i + 1 < cuts.length; i++) {
    const [a, b] = [cuts[i], cuts[i + 1]];
    let style: StylePatch = { ...(spans.find((s) => s.start <= a && b <= s.end)?.style || {}) };
    if (a >= start && b <= end) style = { ...style, ...patch };
    if (!Object.keys(style).length) continue;
    const last = out.at(-1);
    if (last && last.end === a && JSON.stringify(last.style) === JSON.stringify(style)) last.end = b;
    else out.push({ start: a, end: b, style });
  }
  c.spans = out;
  return c;
}
export class AltoError extends Error {
  constructor(
    public code: string,
    message: string,
  ) {
    super(message);
    this.name = "AltoError";
  }
}
class Events {
  private listeners = new Map<string, Set<(v: any) => void>>();
  on(name: string, fn: (v: any) => void) {
    let s = this.listeners.get(name);
    if (!s) this.listeners.set(name, (s = new Set()));
    s.add(fn);
    return () => s!.delete(fn);
  }
  emit(name: string, value: any) {
    for (const f of this.listeners.get(name) || []) f(value);
  }
}
function nodeId() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  let timestamp = BigInt(Date.now());
  for (let i = 5; i >= 0; i--) {
    bytes[i] = Number(timestamp & 255n);
    timestamp >>= 8n;
  }
  bytes[6] = (bytes[6] & 15) | 112;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join(
    "",
  );
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
const props = () => ({
  visible: true,
  locked: false,
  opacity: 1,
  blend: "normal",
  transform: [1, 0, 0, 1, 0, 0],
  mask: null,
  mask_enabled: true,
  mask_linked: true,
  mask_transform: [1, 0, 0, 1, 0, 0],
  vector_mask: null,
  clip_path: null,
  clip_to_below: false,
  effects: [],
});
export class TransactionBuilder {
  readonly ops: any[] = [];
  constructor(private doc: DocumentHandle) {}
  addRaster(options: { parent?: string; index?: number; name?: string } = {}) {
    return this.insert("raster", options);
  }
  addGroup(
    options: {
      parent?: string;
      index?: number;
      name?: string;
      isolated?: boolean;
    } = {},
  ) {
    return this.insert("group", options);
  }
  /** Registers an imported font in the document (required before text can use it). */
  addFont(font: FontAsset | { record: any }) {
    this.ops.push({ type: "set_asset", id: font.record.id, record: font.record });
    return font.record.id as string;
  }
  addText(content: TextContent, options: { parent?: string; index?: number; name?: string } = {}) {
    const id = this.insert("text", { ...options, name: options.name || content.text.split("\n")[0].slice(0, 24) || "テキスト" });
    this.ops[this.ops.length - 1].node.kind = { kind: "text", content };
    return id;
  }
  setText(id: string, content: TextContent) { this.ops.push({ type: "set_text", id, content }); }
  addVector(paths:StyledPath[]=[],options:{parent?:string;index?:number;name?:string}={}) {
    const id=this.insert('vector',{...options,name:options.name||'ベクター'});
    this.ops[this.ops.length-1].node.kind={kind:'vector',paths};return id;
  }
  setVector(id:string,paths:StyledPath[]) {this.ops.push({type:'set_vector',id,paths});}
  setVectorMask(id:string,mask:VectorMask|null) {this.ops.push({type:'set_vector_mask',id,mask});}
  setClipPath(id:string,path:ClipPath|null) {this.ops.push({type:'set_clip_path',id,path});}
  addAdjustment(
    adjustment: {
      type: "color_adjust";
      brightness?: number;
      contrast?: number;
      saturation?: number;
    },
    options: { parent?: string; index?: number; name?: string } = {},
  ) {
    const id = this.insert("adjustment", {
      ...options,
      name: options.name || "色調整",
    });
    this.ops[this.ops.length - 1].node.kind = {
      kind: "adjustment",
      adjustment,
    };
    return id;
  }
  setAdjustment(id: string, adjustment: Record<string, unknown>) {
    this.ops.push({ type: "set_adjustment", id, adjustment });
  }
  setMask(id: string, mask: unknown) {
    this.ops.push({ type: "set_mask", layer: id, mask });
  }
  private insert(kind: string, options: any) {
    const id = nodeId();
    const parent = options.parent || this.doc.view.root.id;
    const existing = this.ops.find(
      (o) => o.type === "insert_node" && o.node.id === parent,
    );
    const count = existing ? 0 : this.doc.view.node(parent).children.length;
    const index =
      options.index ??
      count +
        this.ops.filter((o) => o.type === "insert_node" && o.parent === parent)
          .length;
    this.ops.push({
      type: "insert_node",
      node: {
        id,
        parent: null,
        name: options.name || (kind === "group" ? "グループ" : "レイヤー"),
        props: props(),
        kind:
          kind === "group"
            ? {
                kind,
                children: [],
                isolation: options.isolated ? "isolated" : "pass_through",
              }
            : { kind, tiles: { format: "rgba8", tiles: {} } },
      },
      parent,
      index,
    });
    return id;
  }
  addRasterFromAsset(
    asset: AssetRef,
    options: { parent?: string; index?: number; name?: string } = {},
  ) {
    const id = this.addRaster({ ...options, name: options.name || "画像" });
    this.ops[this.ops.length - 1].node.kind.tiles = asset.tiles;
    this.ops.push({
      type: "set_asset",
      id: asset.record.id,
      record: asset.record,
    });
    return id;
  }
  removeNode(id: string) {
    this.ops.push({ type: "remove_node", id });
  }
  moveNode(id: string, parent: string, index: number) {
    this.ops.push({ type: "move_node", id, parent, index });
  }
  setProps(id: string, patch: Record<string, unknown>) {
    this.ops.push({
      type: "set_props",
      id,
      patch: {
        name: null,
        visible: null,
        locked: null,
        opacity: null,
        blend: null,
        transform: null,
        clip_to_below: null,
        effects: null,
        ...patch,
      },
    });
  }
  setTransform(
    id: string,
    t:
      | number[]
      | {
          x: number;
          y: number;
          scaleX: number;
          scaleY: number;
          rotation: number;
        },
  ) {
    const m = Array.isArray(t)
      ? t
      : [
          Math.cos(t.rotation) * t.scaleX,
          Math.sin(t.rotation) * t.scaleX,
          -Math.sin(t.rotation) * t.scaleY,
          Math.cos(t.rotation) * t.scaleY,
          t.x,
          t.y,
        ];
    this.setProps(id, { transform: m });
  }
}
class DocumentView extends Events {
  private mirror!: Mirror;
  history = { undo: 0, redo: 0 };
  snapshotHash = "";
  stateHash = "";
  versions: VersionStatus = { head: null, commits: 0, changed: false, changes: 0, pending_intents: 0 };
  dirty = false;
  warnings: string[] = [];
  apply(state: any) {
    if (state.patch) {
      this.mirror.revision = state.revision;
      if (Object.prototype.hasOwnProperty.call(state, "meta")) this.mirror.meta = state.meta;
      if (Object.prototype.hasOwnProperty.call(state, "selection"))
        this.mirror.selection = state.selection;
      if (Object.prototype.hasOwnProperty.call(state, "assets"))
        (this.mirror as any).assets = state.assets;
      for (const id of state.removed ?? []) delete this.mirror.nodes[id];
      Object.assign(this.mirror.nodes, state.nodes ?? {});
    } else {
      this.mirror = state.document;
    }
    this.history = state.history;
    this.snapshotHash = state.snapshotHash;
    this.stateHash = state.stateHash;
    this.versions = state.versions;
    const old = this.dirty;
    this.dirty = state.dirty;
    this.warnings = state.warnings || [];
    this.emit("change", state);
    if (old !== this.dirty) this.emit("dirty", this.dirty);
  }
  get selection() {
    return structuredClone(this.mirror.selection);
  }
  /** Asset records (images, fonts) registered in the document. */
  get assets(): any[] {
    return structuredClone(Object.values((this.mirror as any).assets || {}));
  }
  get root() {
    return this.node(this.mirror.root);
  }
  get meta() {
    return Object.freeze({ ...this.mirror.meta });
  }
  get revision() {
    return this.mirror.revision;
  }
  node(id: string): NodeView {
    const n = this.mirror.nodes[id];
    if (!n) throw new AltoError("NOT_FOUND", "レイヤーが見つかりません");
    const children = (n.kind.children || []).map((id) => this.node(id));
    const copy = { ...structuredClone(n), children };
    const freeze = (v: any): any => {
      if (v && typeof v === "object") {
        Object.values(v).forEach(freeze);
        Object.freeze(v);
      }
      return v;
    };
    return freeze(copy) as NodeView;
  }
  get nodes() {
    return Object.values(this.mirror.nodes).map((n) => this.node(n.id));
  }
}
export class ToolSession {
  private samples: number[] = [];
  private ended = false;
  private ready: Promise<any>;
  constructor(
    private doc: DocumentHandle,
    tool: string,
    options: BrushOptions,
  ) {
    this.ready = doc.engine.request("beginBrush", {
      layer: options.layer,
      config: {
        size: options.tip?.size ?? 24,
        hardness: options.tip?.hardness ?? 0.8,
        spacing: options.tip?.spacing ?? 0.15,
        opacity: options.opacity ?? 1,
        flow: options.flow ?? 1,
        pressure_size: options.pressureSize ?? true,
        pressure_opacity: options.pressureOpacity ?? false,
      },
      color: options.color ?? [0, 0, 0, 1],
      erase: tool === "eraser",
      mask: options.target === "mask",
    });
    this.ready.catch((e) => doc.engine.emit("error", e));
  }
  input(
    input:
      | PointerEvent
      | {
          x: number;
          y: number;
          pressure?: number;
          tiltX?: number;
          tiltY?: number;
          time?: number;
        },
  ) {
    if (this.ended) return;
    if ("clientX" in input) {
      const events = input.getCoalescedEvents?.() || [input];
      for (const e of events.length ? events : [input]) {
        const p = this.doc.engine.viewport.screenToDocument(
          e.clientX,
          e.clientY,
        );
        this.samples.push(
          p[0],
          p[1],
          e.pointerType === "mouse" ? 1 : e.pressure,
          e.tiltX,
          e.tiltY,
          e.timeStamp,
        );
      }
    } else
      this.samples.push(
        input.x,
        input.y,
        input.pressure ?? 1,
        input.tiltX ?? 0,
        input.tiltY ?? 0,
        input.time ?? performance.now(),
      );
  }
  async flush() {
    await this.ready;
    if (!this.samples.length) return;
    const a = new Float32Array(this.samples);
    this.samples = [];
    this.doc.engine.stream({ t: "input", samples: a }, [a.buffer]);
  }
  async end() {
    if (this.ended) return;
    this.ended = true;
    await this.flush();
    try {
      const state = await this.doc.engine.request("endBrush");
      this.doc.apply(state);
      return state.document?.revision ?? state.revision;
    } finally {
      this.doc.activeTool = null;
    }
  }
  async cancel() {
    this.ended = true;
    this.samples = [];
    await this.ready;
    await this.doc.engine.request("cancelBrush");
    this.doc.activeTool = null;
  }
}
export class DocumentHandle {
  readonly view = new DocumentView();
  activeTool: ToolSession | null = null;
  /** The folder this document was opened from or last saved to, and its manifest hash. */
  folder: { provider: ObjectStorageProvider; manifest: string } | null = null;
  constructor(
    readonly engine: AltoEngine,
    state: any,
  ) {
    this.apply(state);
  }
  apply(s: any) {
    this.view.apply(s);
    this.engine.emit("change", s);
    this.engine.emit("dirty", s.dirty);
  }
  private textOf(id: string): TextContent {
    const n = this.view.node(id);
    if (n.kind.kind !== "text") throw new AltoError("INVALID_ARGUMENT", "テキストレイヤーではありません");
    return structuredClone(n.kind.content as TextContent);
  }
  readonly text = {
    set: (id: string, content: TextContent) => this.transact("テキスト編集", (tx) => tx.setText(id, content)),
    /** Replaces a UTF-16 range of the text in one undoable edit. */
    replace: (id: string, start: number, end: number, insert: string, intent = "文字入力") =>
      this.transact(intent, (tx) => tx.setText(id, replaceText(this.textOf(id), start, end, insert))),
    style: (id: string, start: number, end: number, patch: StylePatch) =>
      this.transact("文字書式", (tx) => tx.setText(id, applyTextStyle(this.textOf(id), start, end, patch))),
    setTateChuYoko: (id: string, start: number, end: number, on = true) =>
      this.transact("縦中横", (tx) => {
        const c = this.textOf(id);
        const rest = (c.tcy || []).filter((r) => r.end <= start || r.start >= end);
        c.tcy = on ? [...rest, { start, end }].sort((a, b) => a.start - b.start) : rest;
        tx.setText(id, c);
      }),
    info: (id: string): Promise<TextInfo> => this.engine.request("textInfo", { node: id }),
    /** UTF-16 offset nearest to a document point. */
    hitTest: (id: string, x: number, y: number): Promise<number> => this.engine.request("textHitTest", { node: id, x, y }),
    caret: (id: string, offset: number): Promise<TextCaret | null> => this.engine.request("textCaret", { node: id, offset }),
    /** Highlight quads in document coordinates. */
    selectionRects: (id: string, start: number, end: number): Promise<[number, number][][]> =>
      this.engine.request("textSelection", { node: id, start, end }),
    navigate: (id: string, offset: number, motion: TextMotion): Promise<number> =>
      this.engine.request("textNavigate", { node: id, offset, motion }),
  };
  readonly versions = {
    /** Records the current document as a version. Fails when nothing changed unless allowEmpty. */
    commit: async (message: string, options: { author?: string; allowEmpty?: boolean } = {}): Promise<CommitView> => {
      if (this.activeTool) await this.activeTool.end();
      return this.engine.mutate(async () => {
        const r = await this.engine.request("commit", { message, author: options.author || "", allowEmpty: !!options.allowEmpty, time: Date.now() });
        this.apply(r.state);
        return r.commit;
      });
    },
    log: (): Promise<CommitView[]> => this.engine.request("versionLog"),
    /** Semantic, layer-level diff. Use "working" for the current document. */
    diff: (from: string, to: string = "working"): Promise<VersionDiff> => this.engine.request("versionDiff", { from, to }),
    status: () => this.view.versions,
    /** Restores a version as one undoable edit; history is kept. */
    restore: (hash: string) => this.command("restoreVersion", { hash }),
  };
  /** Saves the folder form. Content-addressed files are written once; manifest.json last. */
  async saveFolder(provider: ObjectStorageProvider, options: { overwrite?: boolean } = {}) {
    if (this.activeTool) await this.activeTool.end();
    this.engine.emit("saveState", "saving");
    try {
      const current = await provider.readRef("manifest.json");
      const currentHash = current ? await sha256Hex(current) : "";
      const base = this.folder?.provider === provider ? this.folder.manifest : undefined;
      if (current && base === undefined && !options.overwrite)
        throw new AltoError("CONFLICT", "フォルダには既に別の文書があります");
      if (base !== undefined && base !== currentHash)
        throw new AltoError("CONFLICT", "フォルダの manifest.json が他で変更されています");
      const saved = await this.engine.request("folderEntries");
      const entries = unframeEntries(new Uint8Array(saved.bytes));
      const manifest = entries.get("manifest.json");
      if (!manifest) throw new AltoError("INTERNAL", "manifest missing");
      const objects = [...entries].filter(([p]) => immutable(p)).map(([path, data]) => ({ path, data }));
      const report = await provider.putObjects(objects);
      for (const [p, d] of entries) if (!immutable(p) && p !== "manifest.json") await provider.writeRef(p, d);
      await provider.writeRef("manifest.json", manifest, base ?? currentHash);
      const check = await provider.readRef("manifest.json");
      if (!check || check.length !== manifest.length || check.some((b, i) => b !== manifest[i]))
        throw new AltoError("STORAGE", "manifest の検証に失敗しました");
      this.folder = { provider, manifest: await sha256Hex(manifest) };
      this.apply(await this.engine.request("saved", { token: saved.token }));
      this.engine.emit("saveState", "saved");
      return report;
    } catch (e) {
      this.engine.emit("saveState", "error");
      throw e;
    }
  }
  readonly vectors = {
    set:(id:string,paths:StyledPath[])=>this.transact('Edit vector paths',tx=>tx.setVector(id,paths)),
    setMask:(id:string,mask:VectorMask|null)=>this.transact('Vector mask',tx=>tx.setVectorMask(id,mask)),
    setClipPath:(id:string,path:ClipPath|null)=>this.transact('Clip path',tx=>tx.setClipPath(id,path)),
  };
  readonly tools = {
    begin: (tool: "brush" | "eraser", options: BrushOptions) => {
      if (this.activeTool)
        throw new AltoError("CONFLICT", "ツールが使用中です");
      return (this.activeTool = new ToolSession(this, tool, options));
    },
  };
  async transact<T>(
    intent: string,
    callback: (tx: TransactionBuilder) => T,
  ): Promise<T> {
    if (this.activeTool) await this.activeTool.end();
    return this.engine.mutate(async () => {
      const tx = new TransactionBuilder(this);
      const value = callback(tx);
      if (value && typeof (value as any).then === "function")
        throw new AltoError(
          "INVALID_ARGUMENT",
          "Transaction callback must be synchronous",
        );
      const state = await this.engine.request("transact", {
        intent,
        ops: tx.ops,
        baseRevision: this.view.revision,
      });
      this.apply(state);
      return value;
    });
  }
  async undo() {
    if (this.activeTool) await this.activeTool.end();
    return this.engine.mutate(async () =>
      this.apply(await this.engine.request("undo")),
    );
  }
  async redo() {
    if (this.activeTool) await this.activeTool.end();
    return this.engine.mutate(async () =>
      this.apply(await this.engine.request("redo")),
    );
  }
  async command(type: string, payload: any) {
    if (this.activeTool) await this.activeTool.end();
    return this.engine.mutate(async () => {
      this.apply(await this.engine.request(type, payload));
    });
  }
  readonly selection = {
    apply: (spec: {
      kind: "rectangle" | "ellipse" | "lasso" | "all" | "invert" | "clear";
      mode?: "replace" | "add" | "subtract" | "intersect";
      x?: number;
      y?: number;
      width?: number;
      height?: number;
      feather?: number;
      points?: number[][];
    }) => this.command("select", { spec }),
    outline: () => this.engine.request("selectionOutline") as Promise<string>,
    clear: () => this.command("select", { spec: { kind: "clear" } }),
    invert: () => this.command("select", { spec: { kind: "invert" } }),
    all: () => this.command("select", { spec: { kind: "all" } }),
  };
  readonly masks = {
    fromSelection: (layer: NodeId) =>
      this.command("setMask", { layer, source: "selection" }),
    addWhite: (layer: NodeId) =>
      this.command("setMask", { layer, source: "white" }),
    remove: (layer: NodeId) =>
      this.command("setMask", { layer, source: "remove" }),
  };
  readonly effects = {
    set: (
      layer: NodeId,
      effects: Record<string, unknown>[],
      options: { useSelection?: boolean } = {},
    ) =>
      this.command("setEffects", {
        layer,
        effects,
        useSelection: options.useSelection ?? true,
      }),
  };
  async save(target?: ArchiveStorageProvider | any) {
    if (this.activeTool) await this.activeTool.end();
    this.engine.emit("saveState", "saving");
    try {
      const saved = await this.engine.request("save");
      const data = new Uint8Array(saved.bytes);
      if (!target)
        target = await this.engine.storage.pickSaveFile({
          suggestedName: "work.alto",
        });
      if (target.writeArchive)
        await target.writeArchive(new Blob([data]).stream());
      else {
        const writer = await target.createWritable();
        try {
          await writer.write(data);
          await writer.close();
        } catch (e) {
          await writer.abort();
          throw e;
        }
      }
      this.apply(await this.engine.request("saved", { token: saved.token }));
      this.engine.emit("saveState", "saved");
      return target;
    } catch (e) {
      this.engine.emit("saveState", "error");
      throw e;
    }
  }
}
export class AltoEngine extends Events {
  canvas: HTMLCanvasElement;
  private worker!: Worker;
  private sequence = 1;
  private pending = new Map<
    number,
    { resolve: (v: any) => void; reject: (e: any) => void }
  >();
  private mutation: Promise<any> = Promise.resolve();
  private raf = 0;
  private disposed = false;
  private releaseLease?: () => void;
  private onPageHide = () => this.dispose();
  private boot: any;
  private options: any;
  document: DocumentHandle | null = null;
  recoveryAvailable = false;
  private constructor(options: any) {
    super();
    this.options = options;
    this.canvas = options.canvas;
    window.addEventListener("pagehide", this.onPageHide);
  }
  static async create(options: {
    canvas: HTMLCanvasElement;
    gpu?: { powerPreference?: string; memoryBudgetMB?: number };
    workerURL?: URL | string;
  }) {
    if (!options.canvas.transferControlToOffscreen || !navigator.gpu)
      throw new AltoError("GPU_UNSUPPORTED", "WebGPU対応ブラウザが必要です");
    const e = new AltoEngine(options);
    try {
      await e.acquireLease();
      await e.start();
      return e;
    } catch (error) {
      e.dispose();
      throw error;
    }
  }
  private async acquireLease() {
    if (!navigator.locks) {
      throw new AltoError(
        "CONFLICT",
        "このブラウザは Web Locks が無く、同じ文書を別タブと同時に開けません",
      );
    }
    let release!: () => void;
    const hold = new Promise<void>((resolve) => {
      release = resolve;
    });
    await new Promise<void>((resolve, reject) => {
      void navigator.locks
        .request(
          "alto-engine-working-journal",
          { ifAvailable: true },
          async (lock) => {
            if (!lock) {
              reject(
                new AltoError(
                  "CONFLICT",
                  "同じサイトのAlto Engineが別のタブで動作しています。先にそのタブを閉じてください。",
                ),
              );
              return;
            }
            this.releaseLease = release;
            resolve();
            await hold;
          },
        )
        .catch(reject);
    });
  }
  private async start() {
    this.worker = new Worker(
      this.options.workerURL || new URL("./worker.js", import.meta.url),
      { type: "module" },
    );
    this.worker.onmessage = ({ data }) => {
      if (data.t === "reply") {
        const p = this.pending.get(data.id);
        if (!p) return;
        this.pending.delete(data.id);
        data.ok
          ? p.resolve(data.value)
          : p.reject(new AltoError(data.error.code, data.error.message));
      } else {
        if (data.t === "patch") {
          this.document?.apply(data.value);
        } else if (data.t === "frame") this.emit("frame", data.value);
        else if (data.t === "gpuLost") {
          if (this.document) this.document.activeTool = null;
          this.emit("gpuLost", null);
        } else this.emit(data.t, data.value);
      }
    };
    this.worker.onerror = (event) => {
      const e = new AltoError(
        "INTERNAL",
        JSON.stringify({
          message: event.message,
          file: event.filename,
          line: event.lineno,
          error: String(event.error),
        }),
      );
      console.error(e.message);
      this.emit("error", e);
      for (const p of this.pending.values()) p.reject(e);
      this.pending.clear();
      if (this.boot) void this.restart();
    };
    const offscreen = this.canvas.transferControlToOffscreen();
    this.boot = await this.request(
      "init",
      {
        protocol: 4,
        sdk: "0.4.0",
        canvas: offscreen,
        budget: this.options.gpu?.memoryBudgetMB ?? 512,
      },
      [offscreen],
    );
    if (this.boot.protocol !== 4)
      throw new AltoError("CONFLICT", "SDK/WASM protocol mismatch");
    this.recoveryAvailable = !!this.boot.recovery.available;
    if (this.recoveryAvailable)
      this.emit("recoveryAvailable", this.boot.recovery);
    this.resize();
    const loop = () => {
      if (this.disposed) return;
      void this.document?.activeTool
        ?.flush()
        .catch((e) => this.emit("error", e));
      this.stream({ t: "tick", time: performance.now() });
      this.raf = requestAnimationFrame(loop);
    };
    this.raf = requestAnimationFrame(loop);
  }
  request(
    t: string,
    payload: any = {},
    transfer: Transferable[] = [],
  ): Promise<any> {
    const id = this.sequence++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      // Request fields come last so a payload can never replace the reply id.
      this.worker.postMessage({ ...payload, t, id }, transfer);
    });
  }
  stream(message: any, transfer: Transferable[] = []) {
    this.worker.postMessage(message, transfer);
  }
  mutate<T>(fn: () => Promise<T>): Promise<T> {
    const p = this.mutation.then(fn);
    this.mutation = p.catch(() => {});
    return p;
  }
  resize() {
    const r = this.canvas.getBoundingClientRect();
    const ratio = Math.min(devicePixelRatio || 1, 2);
    this.stream({
      t: "resize",
      width: Math.max(1, Math.round(r.width * ratio)),
      height: Math.max(1, Math.round(r.height * ratio)),
    });
  }
  async createDocument(meta: { width: number; height: number }) {
    const state = await this.request("createDocument", meta);
    this.document = new DocumentHandle(this, state);
    this.viewport.set({
      center: [meta.width / 2, meta.height / 2],
      zoom: 1,
      rotation: 0,
    });
    return this.document;
  }
  async openDocument(source: Blob | ArchiveStorageProvider | any) {
    let blob: Blob;
    if (source instanceof Blob) blob = source;
    else if (source.readArchive) blob = await source.readArchive();
    else blob = await source.getFile();
    const bytes = await blob.arrayBuffer();
    const state = await this.request("open", { bytes }, [bytes]);
    this.document = new DocumentHandle(this, state);
    this.viewport.set({
      center: [state.document.meta.width / 2, state.document.meta.height / 2],
      zoom: 1,
      rotation: 0,
    });
    return this.document;
  }
  /** Opens the folder form, reading only objects reachable from the document and its history. */
  async openFolder(provider: ObjectStorageProvider) {
    const manifest = await provider.readRef("manifest.json");
    if (!manifest) throw new AltoError("NOT_FOUND", "manifest.json がありません");
    const entries = new Map<string, Uint8Array>([["manifest.json", manifest]]);
    for (const p of [...(await provider.list("snapshots/")), ...(await provider.list("commits/"))]) {
      const d = await provider.getObject(p);
      if (d) entries.set(p, d);
    }
    const meta = frameEntries(entries);
    const plan: string[] = await this.request("folderPlan", { bytes: meta.buffer }, [meta.buffer]);
    for (const p of plan) {
      const d = await provider.getObject(p);
      if (d) entries.set(p, d);
    }
    const all = frameEntries(entries);
    const state = await this.request("openFolder", { bytes: all.buffer }, [all.buffer]);
    this.document = new DocumentHandle(this, state);
    this.document.folder = { provider, manifest: await sha256Hex(manifest) };
    this.viewport.set({ center: [state.document.meta.width / 2, state.document.meta.height / 2], zoom: 1, rotation: 0 });
    return this.document;
  }
  async recover() {
    const state = await this.request("recover");
    this.document = new DocumentHandle(this, state);
    this.recoveryAvailable = false;
    this.viewport.set({
      center: [state.document.meta.width / 2, state.document.meta.height / 2],
      zoom: 1,
      rotation: 0,
    });
    return this.document;
  }
  readonly assets = {
    /** Imports an OpenType/TrueType font (.otf/.ttf/.ttc). Add it with tx.addFont(). */
    importFont: async (file: Blob, options: { faceIndex?: number } = {}): Promise<FontAsset> => {
      const bytes = await file.arrayBuffer();
      return this.request("importFont", { bytes, faceIndex: options.faceIndex ?? 0 }, [bytes]);
    },
    importImage: async (file: Blob): Promise<AssetRef> => {
      const bitmap = await createImageBitmap(file);
      const c = new OffscreenCanvas(bitmap.width, bitmap.height);
      const ctx = c.getContext("2d", { willReadFrequently: true });
      if (!ctx) throw new AltoError("INTERNAL", "画像デコード失敗");
      ctx.drawImage(bitmap, 0, 0);
      bitmap.close();
      const pixels = ctx.getImageData(0, 0, c.width, c.height).data;
      const encoded = await file.arrayBuffer();
      return this.request(
        "import",
        {
          width: c.width,
          height: c.height,
          pixels: pixels.buffer,
          encoded,
          mime: file.type || "image/png",
        },
        [pixels.buffer, encoded],
      );
    },
  };
  readonly viewport = {
    version: 0,
    center: [512, 384] as [number, number],
    zoom: 1,
    rotation: 0,
    set: (v: {
      center?: [number, number];
      zoom?: number;
      rotation?: number;
    }) => {
      if (v.center) this.viewport.center = v.center;
      if (v.zoom !== undefined) this.viewport.zoom = v.zoom;
      if (v.rotation !== undefined) this.viewport.rotation = v.rotation;
      this.viewport.version++;
      this.emit("viewport", this.viewport);
      this.stream({
        t: "viewport",
        version: this.viewport.version,
        center: this.viewport.center,
        zoom: this.viewport.zoom,
        rotation: this.viewport.rotation,
      });
    },
    screenToDocument: (x: number, y: number): [number, number] => {
      const r = this.canvas.getBoundingClientRect();
      const ratio = Math.min(devicePixelRatio || 1, 2);
      const dx = ((x - r.left - r.width / 2) * ratio) / this.viewport.zoom;
      const dy = ((y - r.top - r.height / 2) * ratio) / this.viewport.zoom;
      const c = Math.cos(this.viewport.rotation),
        s = Math.sin(this.viewport.rotation);
      return [
        this.viewport.center[0] + c * dx + s * dy,
        this.viewport.center[1] - s * dx + c * dy,
      ];
    },
  };
  readonly storage = {
    /** Picks a folder for the `.altoproj` form (File System Access API). */
    pickFolder: async (): Promise<ObjectStorageProvider> => {
      if (!("showDirectoryPicker" in window))
        throw new AltoError("STORAGE", "このブラウザはフォルダへの保存に対応していません");
      return new FileSystemDirectoryProvider(await (window as any).showDirectoryPicker({ mode: "readwrite" }));
    },
    /** A folder inside the origin private file system. */
    opfsFolder: async (name: string): Promise<ObjectStorageProvider> => {
      const root = await navigator.storage.getDirectory();
      return new FileSystemDirectoryProvider(await root.getDirectoryHandle(name, { create: true }));
    },
    pickSaveFile: async (options: { suggestedName: string }) => {
      if ("showSaveFilePicker" in window)
        return (window as any).showSaveFilePicker({
          suggestedName: options.suggestedName,
          types: [
            {
              description: "Alto document",
              accept: { "application/zip": [".alto"] },
            },
          ],
        });
      return {
        kind: "download",
        writeArchive: async (stream: ReadableStream<Uint8Array>) => {
          const blob = await new Response(stream).blob();
          const a = document.createElement("a");
          a.href = URL.createObjectURL(blob);
          a.download = options.suggestedName;
          a.click();
          setTimeout(() => URL.revokeObjectURL(a.href), 1000);
        },
      };
    },
  };
  readonly vectors = {
    parseSvg:(svg:string):Promise<PathData>=>this.request('parseVectorPath',{svg}),
    toSvg:(data:PathData):Promise<string>=>this.request('vectorPathSvg',{data}),
  };
  readonly unstable = {
    settle: () => this.request("waitFrame"),
    readTile: async (x: number, y: number) =>
      new Uint8Array(await this.request("readTile", { x, y })),
    readTileLevel: async (level:number,x:number,y:number)=>new Uint8Array(await this.request("readTileLevel",{level,x,y})),
    vectorStats:()=>this.request("vectorStats"),
    /** Small text through the glyph atlas (default) or always through the vector path. */
    setGlyphAtlas:(enabled:boolean)=>this.request("setGlyphAtlas",{enabled}),
    destroyGpu: () => this.request("destroyGpu"),
    crash: () => this.restart(),
    corruptJournalTail: () => this.request("corruptJournalTail"),
    saveBytes: async () => {
      if (this.document?.activeTool) await this.document.activeTool.end();
      return new Uint8Array((await this.request("save")).bytes);
    },
  };
  private async restart() {
    if (this.disposed) return;
    cancelAnimationFrame(this.raf);
    this.worker.terminate();
    for (const p of this.pending.values())
      p.reject(new AltoError("INTERNAL", "Worker restarted"));
    this.pending.clear();
    const replacement = this.canvas.cloneNode(false) as HTMLCanvasElement;
    this.canvas.replaceWith(replacement);
    this.canvas = replacement;
    await this.start();
    if (this.document || this.recoveryAvailable) await this.recover();
    this.emit("canvasReplaced", replacement);
    this.emit("recovered", null);
  }
  dispose() {
    this.disposed = true;
    cancelAnimationFrame(this.raf);
    this.worker?.terminate();
    this.releaseLease?.();
    this.releaseLease = undefined;
    window.removeEventListener("pagehide", this.onPageHide);
    for (const p of this.pending.values())
      p.reject(new AltoError("INTERNAL", "Engine disposed"));
    this.pending.clear();
  }
}
