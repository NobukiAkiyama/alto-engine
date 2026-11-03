import type { ObjectTarget, CanvasEditResult, ImageBounds, ImageColor, ImageFilter, ImageHistogram, ImageSelection, ImageStroke, GradientOptions, ImageEditResult, LayerEditResult, MergeResolution, MergeResult, GcOptions, GcReport } from "./image-types.js";
import type { EngineCore } from "./engine.js";
import { AltoError } from "./errors.js";
import { intentRecord, type Intent } from "./intents.js";
import type { Args } from "./protocol.js";
import { SnapshotStore, type DocumentChange, type DocumentSnapshot, type WireState } from "./snapshot.js";
import {
  immutable,
  frameEntries,
  sha256Hex,
  unframeEntries,
  type ArchiveTarget,
  type ObjectStorageProvider,
} from "./storage.js";
import { applyTextStyle, replaceText } from "./text-edit.js";
import { ToolSession, type BrushOptions } from "./tools.js";
import { Transaction } from "./transaction.js";
import type {
  ClipPath,
  CommitView,
  Effect,
  NodeId,
  Rect,
  StylePatch,
  StyledPath,
  TextCaret,
  TextContent,
  TextInfo,
  TextMotion,
  TileRef,
  VectorMask,
  VersionDiff,
  VersionStatus,
} from "./types.js";

export interface SelectionSpec {
  kind: "rectangle" | "ellipse" | "lasso" | "all" | "invert" | "clear";
  mode?: "replace" | "add" | "subtract" | "intersect";
  x?: number;
  y?: number;
  width?: number;
  height?: number;
  feather?: number;
  /** Lasso points `[x, y]` in document pixels. */
  points?: [number, number][];
}

export interface ExportImageOptions {
  type?: "image/png" | "image/webp";
  /** Greater than 0 and at most 1 (default 1). */
  scale?: number;
  /** Document pixels; defaults to the whole document. */
  region?: Rect;
}

/**
 * The open document. Read it with `getSnapshot` and `subscribe`; edit it with
 * `transact` and the named helpers. The same object stays valid while the engine
 * restarts its Worker; `Engine.open`, `createDocument` and `recovery.restore`
 * give a new one.
 */
export class DocumentHandle {
  private readonly store = new SnapshotStore();
  private readonly listeners = new Set<(change: DocumentChange) => void>();
  private tool: ToolSession | null = null;
  private savedFolder: { provider: ObjectStorageProvider; manifest: string } | null = null;
  private releaseHold: (() => Promise<void>) | undefined;

  /** @internal */
  constructor(
    private readonly core: EngineCore,
    state: WireState,
  ) {
    this.store.apply(state);
  }

  /** The current state. Unchanged until the next edit, so it is safe to keep. */
  getSnapshot(): DocumentSnapshot {
    return this.store.get();
  }
  /** Calls `listener` after each change with what changed. Returns an unsubscribe function. */
  subscribe(listener: (change: DocumentChange) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }
  /** The stroke in progress, if any. */
  get activeTool(): ToolSession | null {
    return this.tool;
  }
  /** The folder this document was opened from or last saved to. */
  get folder(): ObjectStorageProvider | null {
    return this.savedFolder?.provider ?? null;
  }

  /** @internal */
  apply(state: unknown): void {
    if (this.closed) return;
    const change = this.store.apply(state as WireState);
    if (!change) return;
    for (const listener of [...this.listeners]) listener(change);
  }
  /** @internal The Worker went away; any stroke in progress ends with `error`. */
  abortTool(error: AltoError): void {
    this.tool?.abort(error);
    this.tool = null;
  }
  /** @internal */
  async endTool(): Promise<void> {
    if (this.tool?.active) await this.tool.end();
  }
  /** @internal */
  flushTool(): Promise<void> | undefined {
    return this.tool?.flush();
  }
  private closed = false;
  /** Requests go through here, so a replaced handle cannot touch the new document. */
  private request: EngineCore["request"] = ((...args: Parameters<EngineCore["request"]>) =>
    this.closed
      ? Promise.reject(
          new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" }),
        )
      : this.core.requestForDocument(this, ...args)) as EngineCore["request"];
  /** @internal The engine opened another document; this handle refuses further calls. */
  close(): void {
    this.closed = true;
    void this.releaseHold?.();
    this.releaseHold = undefined;
    this.abortTool(new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" }));
    this.listeners.clear();
  }
  private command<K extends "undo" | "redo" | "select" | "setMask" | "setEffects" | "restoreVersion">(
    type: K,
    payload: Args<K>,
  ): Promise<void> {
    return this.edit(async () => {
      this.apply(await (this.request as (t: K, a: Args<K>) => Promise<unknown>)(type, payload));
    });
  }
  private async edit<T>(fn: () => Promise<T>): Promise<T> {
    if (this.closed)
      throw new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" });
    return this.core.mutate(async () => {
      if (this.closed)
        throw new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" });
      await this.endTool();
      return fn();
    });
  }

  /**
   * Applies one edit. `build` must be synchronous; its return value is returned.
   * Resolves when the edit is applied; `durable()` waits for the recovery journal.
   */
  transact<T>(intent: Intent, build: (tx: Transaction) => T): Promise<T> {
    return this.edit(async () => {
      const tx = new Transaction(this.getSnapshot());
      const value = build(tx);
      if (value && typeof (value as { then?: unknown }).then === "function")
        throw new AltoError("INVALID_ARGUMENT", "the transact callback must be synchronous");
      const state = await this.request("transact", {
        ...intentRecord(intent),
        ops: tx.ops,
        baseRevision: this.getSnapshot().revision,
      });
      this.apply(state);
      return value;
    });
  }
  /** Undoes the last edit. */
  undo(): Promise<void> {
    return this.command("undo", {});
  }
  /** Redoes the last undone edit. */
  redo(): Promise<void> {
    return this.command("redo", {});
  }
  /**
   * Resolves once every edit made so far is in the recovery journal. Edits resolve
   * when applied; this rejects with STORAGE if the journal could not be written.
   */
  durable(): Promise<void> {
    return this.request("durable").then(() => {});
  }
  /** The hash of the snapshot a save would write. Computed on request, not per edit. */
  snapshotHash(): Promise<string> {
    return this.request("snapshotHash", {});
  }
  /** A raster layer's full tile map. */
  nodeTiles(id: NodeId): Promise<TileRef> {
    return this.request("nodeTiles", { node: id });
  }

  /** Brush and eraser strokes. */
  readonly tools = {
    /** Starts a stroke. Only one stroke runs at a time. */
    begin: (tool: "brush" | "eraser", options: BrushOptions): ToolSession => {
      if (this.closed)
        throw new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" });
      const failure = this.core.storageError();
      if (failure) throw failure;
      if (this.tool?.active) throw new AltoError("CONFLICT", "a tool is already active", { reason: "tool-active" });
      const session = new ToolSession(this.core.toolHost(this), tool, options);
      this.tool = session;
      return session;
    },
  };
  /** @internal A stroke finished, with the engine's state after it (null if none). */
  toolEnded(session: ToolSession, state: unknown): void {
    if (this.tool === session) this.tool = null;
    if (state) this.apply(state);
  }

  private textOf(id: NodeId): TextContent {
    const node = this.getSnapshot().nodes.get(id);
    if (!node) throw new AltoError("NOT_FOUND", `no node ${id}`, { node: id });
    if (node.type !== "text") throw new AltoError("INVALID_ARGUMENT", `node ${id} is not text`);
    return structuredClone(node.content);
  }
  /** Text layer editing and layout queries. Offsets are UTF-16 code units. */
  readonly text = {
    set: (id: NodeId, content: TextContent) => this.transact("text.edit", (tx) => tx.setText(id, content)),
    /** Replaces a UTF-16 range of the text in one undoable edit. */
    replace: (id: NodeId, start: number, end: number, insert: string, intent: Intent = "text.input") =>
      this.transact(intent, (tx) => tx.setText(id, replaceText(this.textOf(id), start, end, insert))),
    style: (id: NodeId, start: number, end: number, patch: StylePatch) =>
      this.transact("text.style", (tx) => tx.setText(id, applyTextStyle(this.textOf(id), start, end, patch))),
    setTateChuYoko: (id: NodeId, start: number, end: number, on = true) =>
      this.transact("text.tate-chu-yoko", (tx) => {
        const c = this.textOf(id);
        const rest = (c.tcy || []).filter((r) => r.end <= start || r.start >= end);
        c.tcy = on ? [...rest, { start, end }].sort((a, b) => a.start - b.start) : rest;
        tx.setText(id, c);
      }),
    info: (id: NodeId) => this.request("textInfo", { node: id }),
    /** UTF-16 offset nearest to a document point. */
    hitTest: (id: NodeId, x: number, y: number) =>
      this.request("textHitTest", { node: id, x, y }),
    caret: (id: NodeId, offset: number) =>
      this.request("textCaret", { node: id, offset }),
    /** Highlight quads in document coordinates. */
    selectionRects: (id: NodeId, start: number, end: number) =>
      this.request("textSelection", { node: id, start, end }),
    navigate: (id: NodeId, offset: number, motion: TextMotion) =>
      this.request("textNavigate", { node: id, offset, motion }),
  };
  /** Vector paths, vector masks and clipping paths. */
  readonly vectors = {
    set: (id: NodeId, paths: StyledPath[]) => this.transact("vector.edit", (tx) => tx.setVector(id, paths)),
    setMask: (id: NodeId, mask: VectorMask | null) =>
      this.transact("vector.mask", (tx) => tx.setVectorMask(id, mask)),
    setClipPath: (id: NodeId, path: ClipPath | null) =>
      this.transact("vector.clip", (tx) => tx.setClipPath(id, path)),
  };
  /** The document selection (an R8 mask). */
  readonly selection = {
    apply: (spec: SelectionSpec) => this.command("select", { spec }),
    /** The selection's outline as SVG path data in document coordinates. */
    outline: () => this.request("selectionOutline", {}),
    clear: () => this.command("select", { spec: { kind: "clear" } }),
    invert: () => this.command("select", { spec: { kind: "invert" } }),
    all: () => this.command("select", { spec: { kind: "all" } }),
  };
  /** A layer's pixel mask. */
  readonly masks = {
    fromSelection: (layer: NodeId) => this.command("setMask", { layer, source: "selection" }),
    addWhite: (layer: NodeId) => this.command("setMask", { layer, source: "white" }),
    remove: (layer: NodeId) => this.command("setMask", { layer, source: "remove" }),
  };
  /** A layer's non-destructive effects. */
  readonly effects = {
    /** Replaces a layer's effects. With `useSelection` (default) each is limited to the selection. */
    set: (layer: NodeId, effects: Effect[], options: { useSelection?: boolean } = {}) =>
      this.command("setEffects", { layer, effects, useSelection: options.useSelection ?? true }),
  };
  private imageCommand(edit: Record<string, unknown>): Promise<ImageEditResult> {
    return this.edit(async () => { const r = await this.request("imageEdit", { edit }); this.apply(r.state); return r.result; });
  }
  private canvasCommand(edit: Record<string, unknown>): Promise<CanvasEditResult> {
    return this.edit(async () => { const r = await this.request("canvasEdit", { edit }); this.apply(r.state); return r.result; });
  }
  private layerCommand(edit: Record<string, unknown>): Promise<LayerEditResult> {
    return this.edit(async () => { const r = await this.request("layerEdit", { edit }); this.apply(r.state); return r.result; });
  }
  private historyCommand(command: Record<string, unknown>, target?: ObjectTarget): Promise<MergeResult | null> {
    const run = async () => { const r = await this.request("historyCommand", { command }); this.apply(r.state); return r.result; };
    return this.edit(() => (target ? this.withObjects(target, run) : run()));
  }
  /**
   * @internal Runs `run` with the target's objects in memory, reading any the document left in
   * its folder. They are verified and checkpointed first, so a failure here changes nothing.
   */
  async withObjects<T>(target: ObjectTarget, run: () => Promise<T>): Promise<T> {
    const missing = await this.request("missingObjects", { target });
    if (missing.length) {
      const owner = this.savedFolder?.provider;
      const unavailable = (why: string) => new AltoError("STORAGE", why, { reason: "objects-unavailable" });
      if (!owner) throw unavailable("this version's pixels are in a folder that is not attached; use versions.attachFolder");
      const read = async (storage: ObjectStorageProvider) => {
        const entries = new Map<string, Uint8Array>();
        for (const path of missing) {
          const bytes = await storage.getObject(path);
          if (!bytes) throw unavailable("the folder no longer has " + path);
          entries.set(path, bytes);
        }
        return entries;
      };
      const framed = frameEntries(await (owner.exclusive ? owner.exclusive(read) : read(owner)));
      await this.request("provideObjects", { bytes: framed.buffer as ArrayBuffer, target }, [framed.buffer]);
    }
    try {
      return await run();
    } finally {
      if (missing.length) await this.request("releaseObjects").catch(() => {});
    }
  }
  /** Pixel edits use node-local coordinates; each successful edit is one Undo step. */
  readonly image = {
    importSvg: (svg: string, options: { parent?: NodeId; index?: number } = {}) => this.edit(async () => {
      const snapshot = this.getSnapshot(); const parent = options.parent ?? snapshot.root;
      const group = snapshot.nodes.get(parent);
      const index = options.index ?? (group?.type === "group" ? group.children.length : 0);
      const r = await this.request("importSvg", { svg, parent, index }); this.apply(r.state); return r.result;
    }),
    crop: (node: NodeId, bounds: ImageBounds) => this.imageCommand({ type: "crop", node, bounds }),
    resize: (node: NodeId, width: number, height: number, options: { filter?: ImageFilter } = {}) =>
      this.imageCommand({ type: "resize", node, width, height, filter: options.filter ?? "bilinear" }),
    rotate: (node: NodeId, quarterTurns: 1 | 2 | 3) => this.imageCommand({ type: "rotate", node, quarter_turns: quarterTurns }),
    flip: (node: NodeId, axis: "horizontal" | "vertical") => this.imageCommand({ type: "flip", node, axis }),
    gradient: (node: NodeId, options: GradientOptions) => this.imageCommand({ type: "gradient", node, options }),
    fill: (node: NodeId, point: [number, number], color: ImageColor, options: { tolerance?: number; selection?: ImageSelection } = {}) =>
      this.imageCommand({ type: "fill", node, point, color, tolerance: options.tolerance ?? 0, selection: options.selection ?? "current" }),
    /** Copies the layer's own pixels along a stroke. `source` is sampled for the first point. */
    cloneStamp: (node: NodeId, source: [number, number], stroke: ImageStroke, options: { opacity?: number; selection?: ImageSelection } = {}) =>
      this.imageCommand({ type: "clone_stamp", node, source, stroke: { hardness: 1, ...stroke }, opacity: options.opacity ?? 1, selection: options.selection ?? "current" }),
    /** Carries the pixels under the tip along a stroke. */
    smudge: (node: NodeId, stroke: ImageStroke, options: { strength?: number; selection?: ImageSelection } = {}) =>
      this.imageCommand({ type: "smudge", node, stroke: { hardness: 1, ...stroke }, strength: options.strength ?? 0.5, selection: options.selection ?? "current" }),
    /** Merges a raster into the raster below it. Refused when the result could not look the same. */
    mergeDown: (node: NodeId) => this.layerCommand({ type: "merge_down", node }),
    /**
     * Bakes a raster's whole-pixel offset, turns a vector layer into a raster on the same node, or
     * flattens a group of rasters and vectors. A vector must sit at a whole-pixel position at scale 1.
     */
    rasterize: (node: NodeId) => this.layerCommand({ type: "rasterize", node }),
    histogram: (node: NodeId, range?: ImageBounds): Promise<ImageHistogram> => this.request("imageHistogram", range ? { node, range } : { node }),
    pickColor: (node: NodeId, point: [number, number]) => this.request("pickColor", { node, x: point[0], y: point[1] }),
    pickComposite: (point: [number, number]) => this.request("pickComposite", { x: point[0], y: point[1] }),
  };
  /**
   * Whole-document geometry, separate from the layer-local `image` edits. Each is one Undo
   * step that moves the document's root transform, so layer pixels stay as they are.
   */
  readonly canvas = {
    /** The rectangle becomes the canvas. Content outside it is kept, only no longer visible. */
    crop: (rect: ImageBounds) => this.canvasCommand({ type: "crop", ...rect }),
    /** Scales the whole document to the new canvas size. */
    resize: (width: number, height: number) => this.canvasCommand({ type: "resize", width, height }),
    rotate: (quarterTurns: 1 | 2 | 3) => this.canvasCommand({ type: "rotate", quarter_turns: quarterTurns }),
    flip: (axis: "horizontal" | "vertical") => this.canvasCommand({ type: "flip", axis }),
  };
  /** Version commits, diffs and restores. */
  readonly versions = {
    gc: (options: GcOptions = {}) => this.gcFolder(options),
    /** How many objects this document references are only in its folder. */
    externalObjects: () => this.request("externalObjects"),
    /**
     * Connects the folder holding this document's history, after a page reload recovered the
     * document without it. Refused if the folder lacks an object the document needs.
     */
    attachFolder: (provider: ObjectStorageProvider) => this.edit(async () => {
      const missing = await this.request("missingObjects", { target: { type: "all" } });
      const hold = await provider.holdObjects?.();
      try {
        const load = async (storage: ObjectStorageProvider) => {
          const present = new Set(await storage.list("objects/"));
          if (missing.some((path) => !present.has(path))) throw new AltoError("CONFLICT", "this folder does not hold the document's history", { reason: "folder-occupied" });
          const manifest = await storage.readRef("manifest.json");
          return manifest ? sha256Hex(manifest) : "";
        };
        this.setFolder(provider, await (provider.exclusive ? provider.exclusive(load) : load(provider)), hold);
      } catch (e) { void hold?.(); throw e; }
    }),
    /** Returns a new compact archive. Does not overwrite an existing file. */
    compactArchive: (archive: Blob) => this.edit(async () => {
      const bytes = await archive.arrayBuffer();
      const framed = await this.withObjects({ type: "all" }, () => this.request("compactArchive", { bytes }, [bytes]));
      const entries = unframeEntries(framed); const result = entries.get("archive.alto");
      if (!result) throw new AltoError("INTERNAL", "missing compact archive");
      const report = JSON.parse(new TextDecoder().decode(entries.get("report.json"))) as GcReport;
      return { bytes: result, report };
    }),
    createBranch: (name: string, options: { from?: string; checkout?: boolean } = {}) => this.historyCommand({ type: "create_branch", name, from: options.from ?? null, checkout: options.checkout ?? false }, options.checkout && options.from ? { type: "commit", hash: options.from } : undefined).then(() => {}),
    checkout: (name: string) => this.historyCommand({ type: "checkout", name }, { type: "branch", name }).then(() => {}),
    detach: (hash: string) => this.historyCommand({ type: "detach", hash }, { type: "commit", hash }).then(() => {}),
    deleteBranch: (name: string, options: { force?: boolean } = {}) => this.historyCommand({ type: "delete_branch", name, force: options.force ?? false }).then(() => {}),
    setBranchProtection: (name: string, protectedState: boolean) => this.historyCommand({ type: "protect_branch", name, protected: protectedState }).then(() => {}),
    listBranches: () => this.request("listBranches"),
    createTag: (name: string, hash: string) => this.historyCommand({ type: "create_tag", name, hash }).then(() => {}),
    deleteTag: (name: string) => this.historyCommand({ type: "delete_tag", name }).then(() => {}),
    pin: (hash: string) => this.historyCommand({ type: "pin", hash, pin: true }).then(() => {}),
    unpin: (hash: string) => this.historyCommand({ type: "pin", hash, pin: false }).then(() => {}),
    prepareMerge: (name: string) => this.edit(() => this.withObjects({ type: "merge", name }, () => this.request("prepareMerge", { name }))),
    cancelMerge: () => this.edit(() => this.request("cancelMerge").then(() => {})),
    finishMerge: (token: string, resolutions: MergeResolution[], options: { message?: string; author?: string } = {}) =>
      this.historyCommand({ type: "finish_merge", token, resolutions, message: options.message ?? "Merge", author: options.author ?? null, created_at: Date.now() }, { type: "merge_plan" }).then((r) => {
        if (!r) throw new AltoError("INTERNAL", "merge returned no result"); return r;
      }),
    /** Records the current document as a version. Fails when nothing changed unless allowEmpty. */
    commit: (message: string, options: { author?: string; allowEmpty?: boolean } = {}): Promise<CommitView> =>
      this.edit(async () => {
        const r = await this.request("commit", {
          message,
          author: options.author || "",
          allowEmpty: !!options.allowEmpty,
          time: Date.now(),
        });
        this.apply(r.state);
        return r.commit;
      }),
    log: (options: { mode?: "first_parent" | "dag"; allBranches?: boolean } = {}) => this.request("versionLog", { dag: options.mode === "dag", allBranches: options.allBranches ?? false }),
    /** Semantic, layer-level diff. Use "working" for the current document. */
    diff: (from: string, to = "working") => this.request("versionDiff", { from, to }),
    status: (): VersionStatus => this.getSnapshot().versions,
    /** Restores a version as one undoable edit; history is kept. */
    restore: (hash: string) => this.edit(() => this.withObjects({ type: "commit", hash }, async () => { this.apply(await this.request("restoreVersion", { hash })); })),
    /** Drops versions that cannot be reached from the head. Returns how many. */
    prune: (): Promise<number> =>
      this.edit(async () => {
        const state = await this.request("pruneVersions");
        this.apply(state);
        return state.removed ?? 0;
      }),
  };

  /**
   * The document, or a region of it, as a PNG or WebP image: the pixels the canvas
   * shows, at `scale` (greater than 0, at most 1). Allowed while storage has failed.
   */
  async exportImage(options: ExportImageOptions = {}): Promise<Blob> {
    const scale = options.scale ?? 1;
    if (!(scale > 0 && scale <= 1))
      throw new AltoError("INVALID_ARGUMENT", "scale must be greater than 0 and at most 1", { reason: "scale" });
    const { width, height } = this.getSnapshot().meta;
    const r = options.region ?? { x0: 0, y0: 0, x1: width, y1: height };
    const region = {
      x0: Math.max(0, Math.floor(r.x0)),
      y0: Math.max(0, Math.floor(r.y0)),
      x1: Math.min(width, Math.ceil(r.x1)),
      y1: Math.min(height, Math.ceil(r.y1)),
    };
    if (region.x1 <= region.x0 || region.y1 <= region.y0)
      throw new AltoError("INVALID_ARGUMENT", "region is empty or outside the document");
    await this.endTool();
    return this.request("exportImage", { region, scale, type: options.type ?? "image/png" });
  }
  /**
   * Writes the document as a `.alto` archive. On success the document is no longer dirty.
   * With `history: "omit"` it writes the working document alone and stays dirty. Use that when
   * history that lives in an unreachable folder would otherwise make the save fail.
   */
  async save(target: ArchiveTarget, options: { history?: "all" | "omit" } = {}): Promise<void> {
    await this.endTool();
    if (options.history === "omit") {
      const bytes = await this.request("saveWithoutHistory");
      await target.write(new Blob([bytes as Uint8Array<ArrayBuffer>]).stream());
      return;
    }
    this.core.emit("saveState", "saving");
    let token: number | undefined;
    try {
      // In the edit queue: an operation in progress may be holding objects of its own.
      const saved = await this.edit(() => this.withObjects({ type: "all" }, () => this.request("save")));
      token = saved.token;
      await target.write(new Blob([saved.bytes as Uint8Array<ArrayBuffer>]).stream());
      this.apply(await this.request("saved", { token }));
      token = undefined;
      this.core.emit("saveState", "saved");
    } catch (e) {
      this.releaseSave(token);
      this.core.emit("saveState", "error");
      throw e;
    }
  }
  private gcFolder(options: GcOptions): Promise<GcReport> {
    return this.edit(async () => {
      const owner = this.savedFolder?.provider;
      if (!owner) throw new AltoError("UNSUPPORTED", "save or open a folder before collecting it");
      const dryRun = options.dryRun ?? true;
      const capabilities = owner.gcCapabilities;
      if (!dryRun && (typeof owner.exclusive !== "function" || capabilities?.exclusive !== true || capabilities.atomicManifest !== true || capabilities.deleteObjects !== true))
        throw new AltoError("UNSUPPORTED", "storage does not guarantee exclusive GC and atomic manifest replacement");
      const run = async (storage: ObjectStorageProvider & { deleteObjects?: (paths: string[]) => Promise<void> }) => {
        if (!dryRun && typeof storage.deleteObjects !== "function") throw new AltoError("UNSUPPORTED", "exclusive storage view does not support deletion");
        const readAll = async () => {
          const manifest = await storage.readRef("manifest.json"); if (!manifest) throw new AltoError("CORRUPT_DATA", "missing manifest");
          const entries = new Map<string, Uint8Array>([["manifest.json", manifest]]);
          for (const prefix of ["objects/", "snapshots/", "commits/"]) for (const path of await storage.list(prefix)) {
            const bytes = await storage.getObject(path); if (!bytes) throw new AltoError("CONFLICT", "object changed during GC"); entries.set(path, bytes);
          }
          const after = await storage.readRef("manifest.json");
          if (!after || await sha256Hex(after) !== await sha256Hex(manifest)) throw new AltoError("CONFLICT", "manifest changed during GC");
          return entries;
        };
        const stored = await readAll(); const framed = frameEntries(stored);
        const output = await this.request("gcPlan", { bytes: framed.buffer as ArrayBuffer, dryRun }, [framed.buffer]);
        const entries = unframeEntries(output);
        const plan = JSON.parse(new TextDecoder().decode(entries.get(".gc-plan.json"))) as { report: GcReport; remove: string[] };
        entries.delete(".gc-plan.json");
        if (options.expectedBaseline !== undefined && options.expectedBaseline !== plan.report.baseline) throw new AltoError("CONFLICT", "GC preview expired");
        if (dryRun) return plan.report;
        const manifest = entries.get("manifest.json")!;
        await storage.writeRef(".alto-gc.json", new TextEncoder().encode(JSON.stringify({ phase: "prepared", baseline: plan.report.baseline, remove: plan.remove })));
        await storage.putObjects([...entries].filter(([path]) => immutable(path)).map(([path, data]) => ({ path, data })));
        const adopt = async () => {
          const r = await this.request("historyCommand", { command: { type: "adopt_gc", manifest: new TextDecoder().decode(manifest) } });
          this.apply(r.state); this.savedFolder = { provider: owner, manifest: await sha256Hex(manifest) };
          await this.request("markFolderSaved");
        };
        try { await storage.writeRef("manifest.json", manifest, plan.report.baseline); }
        catch (error) {
          const after = await storage.readRef("manifest.json");
          if (after && await sha256Hex(after) === await sha256Hex(manifest)) await adopt();
          throw error;
        }
        await adopt();
        let removed = 0;
        try { for (const path of plan.remove) { await storage.deleteObjects!([path]); removed++; } }
        catch (error) { throw new AltoError("STORAGE", `GC stopped after ${removed} deletions: ${String(error)}`); }
        const check = frameEntries(await readAll()); await this.request("gcPlan", { bytes: check.buffer as ArrayBuffer, dryRun: true }, [check.buffer]);
        for (const [path, data] of entries) if (!immutable(path) && path !== "manifest.json") {
          try { await storage.writeRef(path, data); } catch { plan.report.warnings.push(`比較用ファイル ${path} を更新できませんでした。正本は保持されています。`); }
        }
        await storage.writeRef(".alto-gc.json", new TextEncoder().encode(JSON.stringify({ phase: "complete", baseline: plan.report.baseline })));
        return plan.report;
      };
      const collect = () => (owner.exclusive ? owner.exclusive(run) : run(owner));
      if (dryRun || !owner.whileNotHeld) return collect();
      // This document's hold steps aside: GC keeps everything it references.
      const held = !!this.releaseHold;
      await this.releaseHold?.(); this.releaseHold = undefined;
      try { return await owner.whileNotHeld(collect); }
      finally { if (held) this.releaseHold = await owner.holdObjects?.(); }
    });
  }
  /**
   * Saves the folder form. Content-addressed files are written once; manifest.json
   * last. A folder holding another document is refused unless `overwrite`.
   */
  async saveFolder(
    provider: ObjectStorageProvider,
    options: { overwrite?: boolean } = {},
  ): Promise<{ written: number; skipped: number; warnings: string[] }> {
    await this.endTool();
    const save = () => (provider.exclusive ? provider.exclusive((storage) => this.saveFolderLocked(provider, storage, options)) : this.saveFolderLocked(provider, provider, options));
    // Before the target's lock: reading from another folder takes that folder's lock.
    return this.savedFolder?.provider === provider ? save() : this.edit(() => this.withObjects({ type: "all" }, save));
  }
  private async saveFolderLocked(owner: ObjectStorageProvider, provider: ObjectStorageProvider, options: { overwrite?: boolean }): Promise<{ written: number; skipped: number; warnings: string[] }> {
    this.core.emit("saveState", "saving");
    let token: number | undefined;
    try {
      const current = await provider.readRef("manifest.json");
      const currentHash = current ? await sha256Hex(current) : "";
      const base = this.savedFolder?.provider === owner ? this.savedFolder.manifest : undefined;
      if (current && base === undefined && !options.overwrite)
        throw new AltoError("CONFLICT", "the folder holds another document", { reason: "folder-occupied" });
      if (base !== undefined && base !== currentHash)
        throw new AltoError("CONFLICT", "manifest.json changed elsewhere", { reason: "folder-changed" });
      const saved = await this.request("folderEntries", { keepExternal: base !== undefined });
      token = saved.token;
      const entries = unframeEntries(new Uint8Array(saved.bytes));
      const manifest = entries.get("manifest.json");
      if (!manifest) throw new AltoError("INTERNAL", "manifest missing");
      const objects = [...entries].filter(([p]) => immutable(p)).map(([path, data]) => ({ path, data }));
      const report = await provider.putObjects(objects);
      for (const [p, d] of entries) if (!immutable(p) && p !== "manifest.json" && p !== "document.json") await provider.writeRef(p, d);
      await provider.writeRef("manifest.json", manifest, base ?? currentHash);
      const check = await provider.readRef("manifest.json");
      if (!check || check.length !== manifest.length || check.some((b, i) => b !== manifest[i]))
        throw new AltoError("STORAGE", "manifest.json did not read back as written");
      const warnings: string[] = [];
      const mirror = entries.get("document.json");
      if (mirror) { try { await provider.writeRef("document.json", mirror); } catch { warnings.push("比較用の文書コピーを更新できませんでした。正本の保存は完了しています。"); } }
      const movedFolder = this.savedFolder?.provider !== owner;
      this.savedFolder = { provider: owner, manifest: await sha256Hex(manifest) };
      if (movedFolder && this.releaseHold) { void this.releaseHold(); this.releaseHold = await owner.holdObjects?.(); }
      this.apply(await this.request("saved", { token }));
      await this.request("markFolderSaved");
      token = undefined;
      this.core.emit("saveState", "saved");
      return { ...report, warnings };
    } catch (e) {
      this.releaseSave(token);
      this.core.emit("saveState", "error");
      throw e;
    }
  }
  /** @internal */
  setFolder(provider: ObjectStorageProvider, manifest: string, releaseHold?: () => Promise<void>): void {
    void this.releaseHold?.();
    this.releaseHold = releaseHold;
    this.savedFolder = { provider, manifest };
  }
  /** A cancelled or failed save no longer needs the Worker's copy of the package. */
  private releaseSave(token: number | undefined) {
    if (token !== undefined) this.request("releaseSave", { token }).catch(() => {});
  }
}
