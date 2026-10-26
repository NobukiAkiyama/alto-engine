import type { EngineCore } from "./engine.js";
import { AltoError } from "./errors.js";
import { intentRecord, type Intent } from "./intents.js";
import type { Args } from "./protocol.js";
import { SnapshotStore, type DocumentChange, type DocumentSnapshot, type WireState } from "./snapshot.js";
import {
  immutable,
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
    this.abortTool(new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" }));
    this.listeners.clear();
  }
  private command<K extends "undo" | "redo" | "select" | "setMask" | "setEffects" | "restoreVersion" | "upgradeRenderVersion">(
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
   * Switches a version-1 document to render version 2 as one undoable edit.
   * Bilinear and rotated samples change. Opening or saving a document does not
   * do this. A document that is already version 2 is rejected.
   */
  upgradeRenderVersion(): Promise<void> {
    return this.command("upgradeRenderVersion", {});
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
  /** Version commits, diffs and restores. */
  readonly versions = {
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
    log: () => this.request("versionLog", {}),
    /** Semantic, layer-level diff. Use "working" for the current document. */
    diff: (from: string, to = "working") => this.request("versionDiff", { from, to }),
    status: (): VersionStatus => this.getSnapshot().versions,
    /** Restores a version as one undoable edit; history is kept. */
    restore: (hash: string) => this.command("restoreVersion", { hash }),
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
  /** Writes the document as a `.alto` archive. On success the document is no longer dirty. */
  async save(target: ArchiveTarget): Promise<void> {
    await this.endTool();
    this.core.emit("saveState", "saving");
    let token: number | undefined;
    try {
      const saved = await this.request("save");
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
  /**
   * Saves the folder form. Content-addressed files are written once; manifest.json
   * last. A folder holding another document is refused unless `overwrite`.
   */
  async saveFolder(
    provider: ObjectStorageProvider,
    options: { overwrite?: boolean } = {},
  ): Promise<{ written: number; skipped: number }> {
    await this.endTool();
    this.core.emit("saveState", "saving");
    let token: number | undefined;
    try {
      const current = await provider.readRef("manifest.json");
      const currentHash = current ? await sha256Hex(current) : "";
      const base = this.savedFolder?.provider === provider ? this.savedFolder.manifest : undefined;
      if (current && base === undefined && !options.overwrite)
        throw new AltoError("CONFLICT", "the folder holds another document", { reason: "folder-occupied" });
      if (base !== undefined && base !== currentHash)
        throw new AltoError("CONFLICT", "manifest.json changed elsewhere", { reason: "folder-changed" });
      const saved = await this.request("folderEntries");
      token = saved.token;
      const entries = unframeEntries(new Uint8Array(saved.bytes));
      const manifest = entries.get("manifest.json");
      if (!manifest) throw new AltoError("INTERNAL", "manifest missing");
      const objects = [...entries].filter(([p]) => immutable(p)).map(([path, data]) => ({ path, data }));
      const report = await provider.putObjects(objects);
      for (const [p, d] of entries) if (!immutable(p) && p !== "manifest.json") await provider.writeRef(p, d);
      await provider.writeRef("manifest.json", manifest, base ?? currentHash);
      const check = await provider.readRef("manifest.json");
      if (!check || check.length !== manifest.length || check.some((b, i) => b !== manifest[i]))
        throw new AltoError("STORAGE", "manifest.json did not read back as written");
      this.savedFolder = { provider, manifest: await sha256Hex(manifest) };
      this.apply(await this.request("saved", { token }));
      token = undefined;
      this.core.emit("saveState", "saved");
      return report;
    } catch (e) {
      this.releaseSave(token);
      this.core.emit("saveState", "error");
      throw e;
    }
  }
  /** @internal */
  setFolder(provider: ObjectStorageProvider, manifest: string): void {
    this.savedFolder = { provider, manifest };
  }
  /** A cancelled or failed save no longer needs the Worker's copy of the package. */
  private releaseSave(token: number | undefined) {
    if (token !== undefined) this.request("releaseSave", { token }).catch(() => {});
  }
}
