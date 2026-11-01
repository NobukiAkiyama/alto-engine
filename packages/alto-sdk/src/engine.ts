import { DocumentHandle } from "./document.js";
import { AltoError, fromWorkerError } from "./errors.js";
import { Emitter } from "./events.js";
import { PROTOCOL, type Args, type DocumentScope, type RequestName, type Result, type StreamMessage, type FrameStats, type WorkerMessage } from "./protocol.js";
import type { WireState } from "./snapshot.js";
import { frameEntries, sha256Hex, type ArchiveSource, type ObjectStorageProvider } from "./storage.js";
import type { ToolHost, ToolSession } from "./tools.js";
import type { ImportedImage } from "./transaction.js";
import type { FontAsset, ImageAsset, PathData } from "./types.js";
import { ViewportControl, type Viewport } from "./viewport.js";

const SDK_VERSION = "0.5.0";

export interface EngineOptions {
  /** The element the engine draws into. The engine adds its own canvas inside it and never replaces the element. */
  container: HTMLElement;
  storage?: {
    /**
     * Separates recovery journals: one engine per name runs at a time, across tabs.
     * 1–128 characters of `A-Z a-z 0-9 . _ - /`; default `default`.
     */
    namespace?: string;
    /** Set false to keep no recovery journal; unsaved edits are then lost in a crash. */
    recovery?: boolean;
  };
  gpu?: { powerPreference?: "low-power" | "high-performance"; memoryBudgetMB?: number };
  /** Where `worker.js` and the WASM files are served. Must be the page's origin. */
  assets?: { baseURL?: string | URL };
  /** Upper bound for the device pixel ratio the canvas renders at (default 2). */
  maxPixelRatio?: number;
}

export type EngineState = "starting" | "ready" | "restarting" | "storage-failed" | "failed" | "disposed";
export interface EngineStatus {
  readonly state: EngineState;
  /** Why the engine is in `storage-failed` or `failed`. */
  readonly error: AltoError | null;
}

/** Logical canvas publication. This is not an OS scanout timestamp. */
export interface PresentationSnapshot {
  readonly phase: "preparing" | "holding" | "refining" | "ready";
  readonly requestedViewVersion: number;
  readonly presentedViewVersion: number | null;
  readonly presentedViewport: Readonly<{ center: readonly [number, number]; zoom: number; rotation: number }> | null;
  readonly sceneGeneration: number;
  readonly presentedGeneration: number;
  readonly documentSession: number;
  readonly deviceEpoch: number;
  readonly surfaceGeneration: number;
  readonly requestedLevel: number;
  readonly admittedLevel: number;
  readonly degradedByBudget: boolean;
  readonly hasStaleFallback: boolean;
  readonly interactionReady: boolean;
}

export interface EngineEvents {
  presentation: PresentationSnapshot;
  status: EngineStatus;
  /** The open document changed to another one (open, create, restore) or closed. */
  document: DocumentHandle | null;
  /** The view moved, zoomed or rotated. */
  viewport: Viewport;
  saveState: "saving" | "saved" | "error";
  /** A recovery journal from an earlier session can be restored. */
  recoveryAvailable: { records: number };
  /** A failure no call is waiting for, such as a stroke's input failing. */
  error: AltoError;
}

/** @internal Events only the unstable and testing entry points see. */
export interface DiagnosticEvents {
  frame: Record<string, unknown>;
  diagnostic: Record<string, unknown>;
  workerDiagnostic: Record<string, unknown>;
  gpuLost: null;
  gpuRestored: null;
}

export interface RecoveryReport {
  document: DocumentHandle;
  beforeRevision: number | null;
  recoveredRevision: number;
  /** Whether the recovered document equals what was on screen before the failure. */
  matchesDisplay: boolean | null;
}

/** An engine bound to one container element. Create it with `createEngine`. */
export interface Engine {
  /** The engine's canvas, inside the container. It is replaced when the Worker restarts. */
  readonly element: HTMLCanvasElement;
  /** What the engine is doing now; the `status` event reports each change. */
  readonly status: EngineStatus;
  /** The open document. The same handle stays valid across Worker restarts. */
  readonly document: DocumentHandle | null;
  /** Where the canvas looks; zoom is in CSS pixels. */
  readonly viewport: ViewportControl;
  readonly presentation: PresentationSnapshot;
  readonly recovery: {
    /** A journal from an earlier session is waiting. */
    readonly available: boolean;
    restore(): Promise<DocumentHandle>;
    /** Deletes the waiting journal, so a new document can start. Refused when a document is open. */
    discard(): Promise<void>;
    /** The live document as a `.alto` archive, for use when storage has failed. */
    exportEmergency(): Promise<Blob>;
    /** Restarts the Worker and replays the confirmed journal after a storage failure. */
    recoverAfterStorageFailure(): Promise<RecoveryReport>;
  };
  readonly assets: {
    importImage(file: Blob): Promise<ImageAsset>;
    /** An OpenType/TrueType font (.otf/.ttf/.ttc). Add it with `Transaction.addFont`. */
    importFont(file: Blob, options?: { faceIndex?: number }): Promise<FontAsset>;
    /** Drops an imported image or font that no edit used. */
    releaseImport(asset: ImageAsset | FontAsset): Promise<boolean>;
  };
  readonly vectors: {
    parseSvg(svg: string): Promise<PathData>;
    toSvg(data: PathData): Promise<string>;
  };
  /** A new empty document. It uses render version 3, the only version. */
  createDocument(meta: { width: number; height: number; renderVersion?: 3 }): Promise<DocumentHandle>;
  /** Opens a `.alto` archive. Refused while a recovery journal waits. */
  open(source: Blob | ArchiveSource): Promise<DocumentHandle>;
  /** Opens the folder form, reading only objects reachable from the document and its history. */
  openFolder(provider: ObjectStorageProvider): Promise<DocumentHandle>;
  /** Retries a failed Worker without discarding the confirmed journal. */
  retry(): Promise<void>;
  /** Waits briefly for journal writes, then stops the Worker and removes the canvas. */
  dispose(): Promise<void>;
  /** Listens for an engine event; returns a function that stops listening. */
  on<K extends keyof EngineEvents>(name: K, listener: (value: EngineEvents[K]) => void): () => void;
}

const readableAfterStorageFailure = new Set([
  "emergencyExport", "save", "view", "waitFrame", "readTile", "readTileLevel",
  "selectionOutline", "textInfo", "textHitTest", "textCaret",
  "textSelection", "textNavigate", "versionLog", "versionDiff",
  "vectorStats", "vectorPathSvg", "releaseSave", "snapshotHash", "nodeTiles", "exportImage",
]);
const NAMESPACE = /^[A-Za-z0-9._/-]{1,128}$/;

type Pending = { resolve: (v: unknown) => void; reject: (e: unknown) => void; timer?: ReturnType<typeof setTimeout> };

/** @internal The engine. Public callers see it through `Engine`. */
export class EngineCore extends Emitter<EngineEvents> implements Engine {
  element: HTMLCanvasElement;
  document: DocumentHandle | null = null;
  readonly viewport: ViewportControl;
  /** @internal */
  readonly diagnostics = new Emitter<DiagnosticEvents>();
  /** @internal */
  readonly namespace: string;
  private worker: Worker | null = null;
  private sequence = 1;
  private pending = new Map<number, Pending>();
  private mutation: Promise<unknown> = Promise.resolve();
  private nextDocumentEpoch = 0;
  private documentEpoch = 0;
  private readonly documentEpochs = new WeakMap<DocumentHandle, number>();
  private requestedViewVersion = 0;
  private deferredView: Extract<StreamMessage, { t: "viewport" }> | null = null;
  private displayState: PresentationSnapshot = Object.freeze({
    phase: "preparing", requestedViewVersion: 0, presentedViewVersion: null,
    presentedViewport: null, sceneGeneration: 0, presentedGeneration: 0,
    documentSession: 0, deviceEpoch: 0, surfaceGeneration: 0,
    requestedLevel: 0, admittedLevel: 0, degradedByBudget: false,
    hasStaleFallback: false, interactionReady: false,
  });
  get presentation(): PresentationSnapshot {
    return this.displayState;
  }
  private publishDisplay(next: PresentationSnapshot) {
    const frozen = Object.freeze(next);
    if (JSON.stringify(frozen) === JSON.stringify(this.displayState)) return;
    this.displayState = frozen;
    this.element.style.visibility = next.presentedViewport ? "visible" : "hidden";
    this.emit("presentation", frozen);
  }
  private resetDisplay() {
    this.publishDisplay({
      ...this.displayState, phase: "preparing", interactionReady: false,
      presentedViewport: null, presentedViewVersion: null,
    });
  }
  private requestView(v: Extract<StreamMessage, { t: "viewport" }>) {
    this.requestedViewVersion = v.version;
    this.publishDisplay({
      ...this.displayState, requestedViewVersion: v.version,
      phase: this.displayState.presentedViewport ? "holding" : "preparing", interactionReady: false,
    });
    if (this.document?.activeTool) this.deferredView = v;
    else this.stream(v);
  }
  private flushDeferredView() {
    if (this.document?.activeTool || !this.deferredView) return;
    const view = this.deferredView;
    this.deferredView = null;
    this.stream(view);
  }
  private acceptDisplay(frame: FrameStats) {
    if (frame.documentEpoch !== this.documentEpoch || !this.document) return;
    const d = frame.display;
    if (!d) return;
    const sameView = frame.viewVersion === this.requestedViewVersion;
    // A submission becomes visible to this API only after its GPU completion callback.
    const has = !!d.hasPresented;
    const phases = ["preparing", "holding", "refining", "ready"] as const;
    const phase = has ? (sameView ? phases[d.phase] : "holding") : "preparing";
    this.publishDisplay({
      phase, requestedViewVersion: this.requestedViewVersion,
      presentedViewVersion: has ? d.presentedViewVersion : null,
      presentedViewport: has ? Object.freeze({
        center: Object.freeze([d.centerX, d.centerY]) as readonly [number, number],
        zoom: d.zoom / this.pixelRatio, rotation: d.rotation,
      }) : null,
      sceneGeneration: d.generation, presentedGeneration: d.presentedGeneration,
      documentSession: d.session, deviceEpoch: d.deviceEpoch, surfaceGeneration: d.surface,
      requestedLevel: d.requestedLevel, admittedLevel: d.admittedLevel,
      degradedByBudget: d.admittedLevel > d.requestedLevel, hasStaleFallback: !!d.stale,
      interactionReady: sameView && !!d.interactionReady,
    });
  }
  private raf = 0;
  private disposed = false;
  private pixelRatio: number;
  private state: EngineStatus = { state: "starting", error: null };
  private workerGeneration = 0;
  private restartPromise: Promise<void> | null = null;
  private releaseLease?: () => void;
  /** Settles once the namespace lock is released, after `releaseLease`. */
  private leaseReleased: Promise<unknown> = Promise.resolve();
  private lastWorkerPanic: string | null = null;
  private initPhase: string | null = null;
  private recoveryWaiting = false;
  private resizeObserver: ResizeObserver;
  private ratioQuery: MediaQueryList | null = null;
  private onRatioChange = () => this.watchPixelRatio();
  /**
   * Leaving the page ends the engine. A page kept in the back/forward cache only
   * pauses: a journaled engine gives up its namespace (another tab may open the
   * document meanwhile) and restarts from the journal when the page returns. An
   * engine without a journal keeps its Worker, which the browser freezes with the page.
   */
  private onPageHide = (event: PageTransitionEvent) => {
    if (!event.persisted) this.shutdown();
    else if (this.journaled) this.suspend();
  };
  private onPageShow = (event: PageTransitionEvent) => {
    if (event.persisted && this.suspended) void this.resume().catch((e) => this.emit("error", e));
  };
  private suspended = false;
  /** @internal Set by `assets.baseURL` or a test; otherwise the bundled Worker is used. */
  workerURL: URL | string | null;

  private constructor(private readonly options: EngineOptions) {
    super();
    this.namespace = options.storage?.namespace ?? "default";
    this.pixelRatio = this.currentRatio();
    this.workerURL = options.assets?.baseURL
      ? new URL("worker.js", new URL(options.assets.baseURL, location.href))
      : null;
    this.element = this.makeCanvas();
    this.element.style.visibility="hidden";
    options.container.appendChild(this.element);
    this.viewport = new ViewportControl({
      rect: () => this.element.getBoundingClientRect(),
      pixelRatio: () => this.pixelRatio,
      documentSize: () => {
        const meta = this.document?.getSnapshot().meta;
        return meta ? [meta.width, meta.height] : null;
      },
      send: (v) =>
        this.requestView({
          t: "viewport",
          version: v.version,
          center: v.center,
          zoom: v.deviceZoom,
          rotation: v.rotation,
        }),
      changed: (v) => this.emit("viewport", v),
    });
    this.resizeObserver = new ResizeObserver(() => this.scheduleResize());
    this.resizeObserver.observe(options.container);
    this.watchPixelRatio();
    addEventListener("pagehide", this.onPageHide);
    addEventListener("pageshow", this.onPageShow);
  }

  /** @internal */
  static async create(options: EngineOptions): Promise<EngineCore> {
    if (!(options.container instanceof HTMLElement))
      throw new AltoError("INVALID_ARGUMENT", "options.container must be an HTMLElement");
    const namespace = options.storage?.namespace ?? "default";
    if (!NAMESPACE.test(namespace) || namespace.split("/").some((part) => part === "." || part === ".."))
      throw new AltoError("INVALID_ARGUMENT", "storage.namespace must be 1–128 of A-Z a-z 0-9 . _ - /", {
        reason: "namespace",
      });
    if (options.assets?.baseURL && new URL(options.assets.baseURL, location.href).origin !== location.origin)
      throw new AltoError("INVALID_ARGUMENT", "assets.baseURL must be on the page's origin (Workers cannot load cross-origin)", {
        reason: "cross-origin-assets",
      });
    if (!("transferControlToOffscreen" in HTMLCanvasElement.prototype) || !navigator.gpu)
      throw new AltoError("GPU_UNSUPPORTED", "this browser has no WebGPU or OffscreenCanvas");
    const e = new EngineCore(options);
    try {
      if (e.journaled) await e.acquireLease();
      await e.start();
      e.setStatus("ready", null);
      e.startLoop();
      return e;
    } catch (error) {
      await e.dispose();
      throw error;
    }
  }

  get status(): EngineStatus {
    return this.state;
  }
  /** @internal */
  get journaled(): boolean {
    return this.options.storage?.recovery !== false;
  }
  /** @internal */
  get pixelRatioValue(): number {
    return this.pixelRatio;
  }
  /** @internal */
  storageError(): AltoError | null {
    return this.state.state === "storage-failed" ? this.state.error : null;
  }
  private setStatus(state: EngineState, error: AltoError | null) {
    if (this.state.state === state && this.state.error === error) return;
    this.state = Object.freeze({ state, error });
    this.emit("status", this.state);
  }
  private currentRatio() {
    return Math.min(devicePixelRatio || 1, this.options.maxPixelRatio ?? 2);
  }
  private makeCanvas() {
    const c = document.createElement("canvas");
    c.style.cssText = "display:block;width:100%;height:100%;touch-action:none;visibility:hidden";
    return c;
  }
  private watchPixelRatio() {
    this.ratioQuery?.removeEventListener("change", this.onRatioChange);
    if (this.disposed) return;
    this.ratioQuery = matchMedia(`(resolution: ${devicePixelRatio || 1}dppx)`);
    this.ratioQuery.addEventListener("change", this.onRatioChange);
    const ratio = this.currentRatio();
    if (ratio !== this.pixelRatio) {
      this.pixelRatio = ratio;
      this.resize();
      this.viewport.publish();
    }
  }

  private async acquireLease() {
    if (!navigator.locks)
      throw new AltoError("CONFLICT", "this browser has no Web Locks, so a journal cannot be held exclusively", {
        reason: "another-tab",
      });
    let release!: () => void;
    const hold = new Promise<void>((resolve) => {
      release = resolve;
    });
    await new Promise<void>((resolve, reject) => {
      this.leaseReleased = navigator.locks
        .request(`alto-engine:${this.namespace}`, { ifAvailable: true }, async (lock) => {
          if (!lock) {
            reject(
              new AltoError("CONFLICT", `storage namespace ${this.namespace} is in use by another engine`, {
                reason: "namespace-in-use",
                namespace: this.namespace,
              }),
            );
            return;
          }
          this.releaseLease = release;
          resolve();
          await hold;
        })
        .catch(reject)
        .catch(() => {});
    });
  }

  private async start() {
    this.initPhase = null;
    // Written out literally so bundlers (Vite, webpack) see the Worker and bundle it.
    const worker = this.workerURL
      ? new Worker(this.workerURL, { type: "module" })
      : new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
    this.worker = worker;
    this.resetDisplay();
    const generation = ++this.workerGeneration;
    worker.onmessage = ({ data }) => {
      if (generation !== this.workerGeneration || this.disposed) return;
      this.onWorkerMessage(data);
    };
    worker.onerror = (event) => {
      if (generation !== this.workerGeneration || this.disposed) return;
      // The Worker reports a Rust panic just before its error reaches here.
      const panic = this.lastWorkerPanic;
      this.lastWorkerPanic = null;
      const running = this.state.state === "ready" || this.state.state === "storage-failed";
      const detail = (event.message || "") + (panic ? " (panic: " + panic + ")" : "");
      const e = new AltoError(
        running ? "WORKER_CRASHED" : "WORKER_UNAVAILABLE",
        detail || (running ? "the Worker stopped" : "the Worker could not be loaded"),
      );
      this.diagnostics.emit("workerDiagnostic", {
        state: this.state.state,
        workerURL: String(this.workerURL ?? new URL("./worker.js", import.meta.url)),
        message: event.message || null,
        file: event.filename || null,
        line: event.lineno || null,
      });
      this.rejectPending(e);
      if (running) void this.restart().catch((failure) => this.emit("error", failure));
    };
    const offscreen = this.element.transferControlToOffscreen();
    const boot = (await this.requestCore<"init">(
      "init",
      {
        protocol: PROTOCOL,
        sdk: SDK_VERSION,
        canvas: offscreen,
        budget: this.options.gpu?.memoryBudgetMB ?? 512,
        namespace: this.namespace,
        journal: this.journaled,
      },
      [offscreen],
      true,
      30000,
    )) as Result<"init">;
    if (this.worker !== worker || this.disposed)
      throw new AltoError("WORKER_UNAVAILABLE", "the Worker start was interrupted");
    if (boot.protocol !== PROTOCOL) throw new AltoError("CONFLICT", "SDK/WASM protocol mismatch");
    this.recoveryWaiting = !!boot.recovery.available;
    if (this.recoveryWaiting) this.emit("recoveryAvailable", { records: boot.recovery.records });
    // A new Worker knows no size yet.
    this.sentSize = "";
    this.resize();
  }

  private onWorkerMessage(data: WorkerMessage) {
    if (data.t === "reply") {
      const p = this.pending.get(data.id);
      if (!p) return;
      this.pending.delete(data.id);
      if (p.timer) clearTimeout(p.timer);
      if (data.ok) p.resolve(data.value);
      else p.reject(fromWorkerError(data.error));
    } else if (data.t === "patch") {
      if(this.document && this.document.getSnapshot().revision!==data.value.revision)
        this.publishDisplay({...this.displayState,phase:this.displayState.presentedViewport ? "holding" : "preparing",interactionReady:false});
      this.document?.apply(data.value);
    } else if (data.t === "storageFailure") {
      if (this.state.state !== "storage-failed") {
        const error = new AltoError("STORAGE", data.value.message);
        this.document?.abortTool(error);
        this.flushDeferredView();
        this.setStatus("storage-failed", error);
      }
    } else if (data.t === "frame") {
      this.acceptDisplay(data.value);
      this.diagnostics.emit("frame", data.value);
    } else if (data.t === "diagnostic") {
      const value = data.value;
      if (value?.panic) this.lastWorkerPanic = String(value.panic);
      if (typeof value?.initPhase === "string") this.initPhase = value.initPhase;
      this.diagnostics.emit("diagnostic", value);
    } else if (data.t === "gpuLost") {
      this.resetDisplay();
      this.document?.abortTool(new AltoError("GPU_LOST", "the GPU device was lost during the stroke"));
      this.flushDeferredView();
      this.diagnostics.emit("gpuLost", null);
    } else if (data.t === "gpuRestored") {
      this.diagnostics.emit("gpuRestored", null);
    } else if (data.t === "error") {
      this.emit("error", fromWorkerError(data.value));
    }
  }

  private startLoop() {
    const generation = this.workerGeneration;
    const loop = () => {
      if (this.disposed || generation !== this.workerGeneration) return;
      if (this.state.state !== "ready" && this.state.state !== "storage-failed") return;
      void this.document?.flushTool()?.catch((e) => this.emit("error", e));
      this.stream({ t: "tick", time: performance.now() });
      this.raf = requestAnimationFrame(loop);
    };
    this.raf = requestAnimationFrame(loop);
  }

  /** @internal A Worker request, typed by `protocol.ts`. Requests without arguments may omit them. */
  request<K extends RequestName>(
    t: K,
    ...rest: Record<string, never> extends Args<K>
      ? [args?: Args<K>, transfer?: Transferable[]]
      : [args: Args<K>, transfer?: Transferable[]]
  ): Promise<Result<K>> {
    const [args, transfer = []] = rest;
    return this.requestCore(t, (args ?? {}) as Args<K>, transfer, false) as Promise<Result<K>>;
  }
  /** @internal Any request by name, for the testing entry point. */
  requestRaw(t: string, payload: Record<string, unknown> = {}): Promise<unknown> {
    return this.requestCore(t as RequestName, payload as never, [], false);
  }
  /** @internal Binds a request to this handle, including across a Worker round trip. */
  requestForDocument<K extends RequestName>(
    doc: DocumentHandle,
    t: K,
    ...rest: Record<string, never> extends Args<K>
      ? [args?: Args<K>, transfer?: Transferable[]]
      : [args: Args<K>, transfer?: Transferable[]]
  ): Promise<Result<K>> {
    const epoch = this.documentEpochs.get(doc);
    if (doc !== this.document || epoch === undefined)
      return Promise.reject(new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" }));
    const [args, transfer = []] = rest;
    return this.requestCore(t, (args ?? {}) as Args<K>, transfer, false, 0, { documentEpoch: epoch }) as Promise<Result<K>>;
  }
  private requestCore<K extends RequestName>(
    t: K,
    payload: Args<K>,
    transfer: Transferable[],
    internal: boolean,
    timeoutMs = 0,
    scope: DocumentScope = {},
  ): Promise<unknown> {
    if (this.disposed) return Promise.reject(new AltoError("WORKER_UNAVAILABLE", "the engine was disposed"));
    const running = this.state.state === "ready" || this.state.state === "storage-failed";
    if (!internal && !running)
      return Promise.reject(this.state.error || new AltoError("WORKER_UNAVAILABLE", "the engine is restarting"));
    const failure = this.storageError();
    if (failure && !internal && !readableAfterStorageFailure.has(t) && !(t === "open" && failure.code === "CORRUPT_DATA"))
      return Promise.reject(failure);
    const worker = this.worker;
    if (!worker) return Promise.reject(new AltoError("WORKER_UNAVAILABLE", "no Worker"));
    const id = this.sequence++;
    return new Promise((resolve, reject) => {
      const timer = timeoutMs
        ? setTimeout(() => {
            this.pending.delete(id);
            const stage = t === "init" && this.initPhase ? ` (last stage: ${this.initPhase})` : "";
            reject(new AltoError("WORKER_TIMEOUT", `no reply to ${t}${stage}`));
          }, timeoutMs)
        : undefined;
      this.pending.set(id, { resolve, reject, timer });
      // Request fields come last so a payload can never replace the reply id.
      try {
        worker.postMessage({ ...payload, ...scope, t, id }, transfer);
      } catch (error) {
        if (timer) clearTimeout(timer);
        this.pending.delete(id);
        reject(error);
      }
    });
  }
  private rejectPending(error: AltoError) {
    for (const p of this.pending.values()) {
      if (p.timer) clearTimeout(p.timer);
      p.reject(error);
    }
    this.pending.clear();
  }
  /** @internal */
  stream(message: StreamMessage, transfer: Transferable[] = []): void {
    if (this.disposed || this.state.state === "failed" || !this.worker) return;
    this.worker.postMessage(message, transfer);
  }
  /** @internal Runs edits one at a time. */
  mutate<T>(fn: () => Promise<T>): Promise<T> {
    return this.serialize(async () => {
      if (this.state.state === "failed" && this.state.error) throw this.state.error;
      const failure = this.storageError();
      if (failure) throw failure;
      return fn();
    });
  }
  /** Document replacement and recovery decisions share the edit order. */
  private serialize<T>(fn: () => Promise<T>): Promise<T> {
    const p = this.mutation.then(fn);
    this.mutation = p.catch(() => {});
    return p;
  }
  /** @internal */
  toolHost(doc: DocumentHandle): ToolHost {
    // Freeze the transform for this stroke, while viewport.get() stays synchronous.
    const view = this.viewport.get(), rect = this.element.getBoundingClientRect();
    const ready = this.presentation.interactionReady;
    return {
      presentationReady: () => ready,
      request: ((...args: Parameters<EngineCore["request"]>) => this.requestForDocument(doc, ...args)) as EngineCore["request"],
      stream: (m, transfer) => {
        if (doc !== this.document) return;
        this.stream(m.t === "input" ? { ...m, documentEpoch: this.documentEpochs.get(doc) } : m, transfer);
      },
      screenToDocument: (x, y) => {
        const dx = (x - rect.left - rect.width / 2) / view.zoom;
        const dy = (y - rect.top - rect.height / 2) / view.zoom;
        const c = Math.cos(view.rotation), s = Math.sin(view.rotation);
        return [view.center[0] + c * dx + s * dy, view.center[1] - s * dx + c * dy];
      },
      storageError: () => this.storageError(),
      ended: (session: ToolSession, state) => {
        doc.toolEnded(session, state);
        if (doc === this.document) this.flushDeferredView();
      },
      reportError: (e) => this.emit("error", e instanceof AltoError ? e : new AltoError("INTERNAL", String(e))),
    };
  }
  private resizeTimer: ReturnType<typeof setTimeout> | null = null;
  /** The canvas size last sent to this Worker, as "w×h"; empty for a new Worker. */
  private sentSize = "";
  /**
   * Resizing a Worker-owned canvas reconfigures its surface, and doing that while the
   * GPU is busy can lose the device in Chrome. A layout that shifts a few pixels each
   * frame would do it each frame, so the container's size is applied once it has held
   * for 100 ms.
   */
  private scheduleResize() {
    if (this.resizeTimer !== null) clearTimeout(this.resizeTimer);
    this.resizeTimer = setTimeout(() => {
      this.resizeTimer = null;
      this.resize();
    }, 100);
  }
  private resize() {
    const r = this.options.container.getBoundingClientRect();
    // A hidden container is 0×0; render 1×1 until it is shown.
    const width = Math.max(1, Math.round(r.width * this.pixelRatio));
    const height = Math.max(1, Math.round(r.height * this.pixelRatio));
    if (`${width}x${height}` === this.sentSize) return;
    this.sentSize = `${width}x${height}`;
    this.stream({ t: "resize", width, height });
  }

  private adopt(state: unknown, fit: boolean, epoch: number): DocumentHandle {
    this.document?.close();
    this.document = new DocumentHandle(this, state as WireState);
    this.documentEpoch = epoch;
    this.documentEpochs.set(this.document, epoch);
    this.deferredView=null;
    this.resetDisplay();
    this.emit("document", this.document);
    if (fit) this.viewport.fit();
    else this.viewport.publish();
    return this.document;
  }
  /** A waiting journal would be overwritten by the new document's first checkpoint. */
  private refuseWhileRecoveryWaits() {
    if (this.recoveryWaiting)
      throw new AltoError("CONFLICT", "a recovery journal is waiting: restore or discard it first", {
        reason: "recovery-pending",
      });
  }
  private replaceDocument(load: (epoch: number) => Promise<unknown>): Promise<DocumentHandle> {
    return this.serialize(async () => {
      this.refuseWhileRecoveryWaits();
      const reopening = this.storageError()?.code === "CORRUPT_DATA";
      const epoch = ++this.nextDocumentEpoch;
      const state = await load(epoch);
      const doc = this.adopt(state, true, epoch);
      if (reopening) this.setStatus("ready", null);
      return doc;
    });
  }
  private async requestReplacement<K extends "createDocument" | "open" | "openFolder" | "recover">(
    t: K,
    args: Args<K>,
    epoch: number,
    transfer: Transferable[] = [],
  ): Promise<Result<K>> {
    // Finish the old stroke before the Worker replaces its document, not after.
    await this.document?.endTool();
    return this.requestCore(t, args, transfer, false, 0, { nextDocumentEpoch: epoch }) as Promise<Result<K>>;
  }
  async createDocument(meta: { width: number; height: number; renderVersion?: 3 }): Promise<DocumentHandle> {
    return this.replaceDocument((epoch) => this.requestReplacement("createDocument", meta, epoch));
  }
  async open(source: Blob | ArchiveSource): Promise<DocumentHandle> {
    return this.replaceDocument(async (epoch) => {
      const blob = source instanceof Blob ? source : await source.read();
      const bytes = await blob.arrayBuffer();
      return this.requestReplacement("open", { bytes }, epoch, [bytes]);
    });
  }
  async openFolder(provider: ObjectStorageProvider): Promise<DocumentHandle> {
    let manifestHash = "";
    const doc = await this.replaceDocument(async (epoch) => {
      const manifest = await provider.readRef("manifest.json");
      if (!manifest) throw new AltoError("NOT_FOUND", "the folder has no manifest.json");
      manifestHash = await sha256Hex(manifest);
      const entries = new Map<string, Uint8Array>([["manifest.json", manifest]]);
      for (const p of [...(await provider.list("snapshots/")), ...(await provider.list("commits/"))]) {
        const d = await provider.getObject(p);
        if (d) entries.set(p, d);
      }
      const meta = frameEntries(entries);
      const plan = await this.request("folderPlan", { bytes: meta.buffer as ArrayBuffer }, [meta.buffer]);
      for (const p of plan) {
        const d = await provider.getObject(p);
        if (d) entries.set(p, d);
      }
      const all = frameEntries(entries);
      return this.requestReplacement("openFolder", { bytes: all.buffer as ArrayBuffer }, epoch, [all.buffer]);
    });
    doc.setFolder(provider, manifestHash);
    return doc;
  }

  readonly recovery = this.makeRecovery();
  private makeRecovery() {
    const self = this;
    return {
      get available(): boolean {
        return self.recoveryWaiting;
      },
      restore: () => self.restoreRecovery(),
      discard: () => self.discardRecovery(),
      exportEmergency: () => self.exportEmergency(),
      recoverAfterStorageFailure: () => self.recoverAfterStorageFailure(),
    };
  }
  private restoreRecovery = (): Promise<DocumentHandle> => this.serialize(async () => {
    if (!this.journaled) throw new AltoError("NOT_FOUND", "the engine keeps no recovery journal");
    this.requireWaitingRecovery();
    const epoch = ++this.nextDocumentEpoch;
    const state = await this.requestReplacement("recover", {}, epoch);
    this.recoveryWaiting = false;
    return this.adopt(state, true, epoch);
  });
  private discardRecovery = (): Promise<void> => this.serialize(async () => {
    this.requireWaitingRecovery();
    await this.request("discardRecovery");
    this.recoveryWaiting = false;
  });
  private requireWaitingRecovery() {
    if (!this.recoveryWaiting || this.document)
      throw new AltoError("CONFLICT", "there is no waiting recovery journal", { reason: "no-recovery" });
  }
  private exportEmergency = async (): Promise<Blob> =>
    new Blob([(await this.request("emergencyExport")) as Uint8Array<ArrayBuffer>]);
  private recoverAfterStorageFailure = async (): Promise<RecoveryReport> => {
    const failure = this.storageError();
    if (!failure) throw new AltoError("CONFLICT", "storage has not failed", { reason: "not-failed" });
    const snapshot = this.document?.getSnapshot();
    const before = snapshot && { revision: snapshot.revision, stateHash: snapshot.stateHash };
    await this.restart();
    if (!this.document) throw new AltoError("NOT_FOUND", "no journal to recover from");
    const after = this.document.getSnapshot();
    return {
      document: this.document,
      beforeRevision: before?.revision ?? null,
      recoveredRevision: after.revision,
      matchesDisplay: before ? before.stateHash === after.stateHash : null,
    };
  };
  readonly assets = {
    importImage: async (file: Blob): Promise<ImageAsset> => {
      const bitmap = await createImageBitmap(file);
      const c = new OffscreenCanvas(bitmap.width, bitmap.height);
      const ctx = c.getContext("2d", { willReadFrequently: true });
      if (!ctx) throw new AltoError("INTERNAL", "no 2D context to decode the image");
      ctx.drawImage(bitmap, 0, 0);
      bitmap.close();
      const pixels = ctx.getImageData(0, 0, c.width, c.height).data;
      const encoded = await file.arrayBuffer();
      const asset: ImportedImage = await this.request(
        "import",
        { width: c.width, height: c.height, pixels: pixels.buffer as ArrayBuffer, encoded, mime: file.type || "image/png" },
        [pixels.buffer, encoded],
      );
      return { ...asset, id: asset.record.id } as unknown as ImageAsset;
    },
    importFont: async (file: Blob, options: { faceIndex?: number } = {}): Promise<FontAsset> => {
      const bytes = await file.arrayBuffer();
      const font = await this.request("importFont", { bytes, faceIndex: options.faceIndex ?? 0 }, [bytes]);
      return { ...font, id: font.record.id } as unknown as FontAsset;
    },
    releaseImport: (asset: ImageAsset | FontAsset): Promise<boolean> => {
      const record = (asset as unknown as { record: { hash: string } }).record;
      return this.request("releaseImport", { hash: record.hash });
    },
  };
  readonly vectors = {
    parseSvg: (svg: string): Promise<PathData> => this.request("parseVectorPath", { svg }),
    toSvg: (data: PathData): Promise<string> => this.request("vectorPathSvg", { data }),
  };

  retry(): Promise<void> {
    if (this.state.state !== "failed")
      return Promise.reject(new AltoError("CONFLICT", "the engine has not failed", { reason: "not-failed" }));
    // A failed return from the back/forward cache left the namespace to someone else.
    const run = async () => {
      if (this.journaled && !this.releaseLease) await this.acquireLease();
      await this.restart();
    };
    const previous = this.restartPromise;
    return previous ? previous.catch(() => {}).then(run) : run();
  }
  /** @internal Stops the Worker and starts a new one with the confirmed journal. */
  restart(): Promise<void> {
    if (this.disposed) return Promise.reject(new AltoError("WORKER_UNAVAILABLE", "the engine was disposed"));
    if (this.restartPromise) return this.restartPromise;
    const task = Promise.resolve().then(() => this.performRestart());
    this.restartPromise = task;
    // After the promise is stored: a listener that asks for a restart shares it.
    this.setStatus("restarting", null);
    const clear = () => {
      if (this.restartPromise === task) this.restartPromise = null;
    };
    void task.then(clear, clear);
    return task;
  }
  private async performRestart(): Promise<void> {
    cancelAnimationFrame(this.raf);
    ++this.workerGeneration;
    this.worker?.terminate();
    const crashed = new AltoError("WORKER_CRASHED", "the Worker was restarted");
    this.document?.abortTool(crashed);
    this.rejectPending(crashed);
    // A canvas hands its control to one Worker only; the new Worker needs a new canvas.
    const replacement = this.makeCanvas();
    this.element.replaceWith(replacement);
    this.element = replacement;
    try {
      await this.start();
      if (this.disposed) throw new AltoError("WORKER_UNAVAILABLE", "the engine was disposed");
      if (this.document || this.recoveryWaiting) {
        const epoch = this.document ? this.documentEpoch : ++this.nextDocumentEpoch;
        const state = await this.requestCore<"recover">("recover", {}, [], true, 30000, { nextDocumentEpoch: epoch });
        if (this.disposed) throw new AltoError("WORKER_UNAVAILABLE", "the engine was disposed");
        this.recoveryWaiting = false;
        // The same handle carries on; the host's references stay valid.
        if (this.document) {
          this.document.apply(state);
          this.viewport.publish();
        } else this.adopt(state, true, epoch);
      }
      this.setStatus("ready", null);
      this.startLoop();
    } catch (cause) {
      if (this.disposed) throw cause;
      const error =
        cause instanceof AltoError
          ? cause
          : new AltoError("WORKER_UNAVAILABLE", cause instanceof Error ? cause.message : String(cause));
      if (error.code === "CORRUPT_DATA") {
        // Neither generation opens; nothing is left to restore.
        this.recoveryWaiting = false;
        this.document?.close();
        this.document = null;
        this.emit("document", null);
        this.setStatus("storage-failed", error);
        this.startLoop();
      } else {
        this.worker?.terminate();
        this.setStatus("failed", error);
      }
      throw error;
    }
  }
  /** The page went into the back/forward cache. */
  private suspend() {
    if (this.disposed || this.suspended) return;
    this.suspended = true;
    cancelAnimationFrame(this.raf);
    ++this.workerGeneration;
    this.worker?.terminate();
    this.worker = null;
    const paused = new AltoError("WORKER_UNAVAILABLE", "the page was put in the back/forward cache");
    this.document?.abortTool(paused);
    this.rejectPending(paused);
    this.releaseLease?.();
    this.releaseLease = undefined;
    this.setStatus("restarting", null);
  }
  /** The page came back from the back/forward cache: take the namespace again and restart. */
  private async resume() {
    this.suspended = false;
    await this.leaseReleased;
    try {
      await this.acquireLease();
    } catch (error) {
      this.setStatus("failed", error instanceof AltoError ? error : new AltoError("CONFLICT", String(error)));
      throw error;
    }
    await this.restart();
  }
  private shutdown() {
    if (this.disposed) return;
    this.disposed = true;
    ++this.workerGeneration;
    cancelAnimationFrame(this.raf);
    this.worker?.terminate();
    this.worker = null;
    this.releaseLease?.();
    this.releaseLease = undefined;
    this.resizeObserver.disconnect();
    if (this.resizeTimer !== null) clearTimeout(this.resizeTimer);
    this.ratioQuery?.removeEventListener("change", this.onRatioChange);
    removeEventListener("pagehide", this.onPageHide);
    removeEventListener("pageshow", this.onPageShow);
    this.document?.abortTool(new AltoError("WORKER_UNAVAILABLE", "the engine was disposed"));
    this.rejectPending(new AltoError("WORKER_UNAVAILABLE", "the engine was disposed"));
    this.element.remove();
    this.setStatus("disposed", null);
  }
  async dispose(): Promise<void> {
    if (this.disposed) return;
    if (this.worker && this.document && this.state.state === "ready") {
      // Edits already applied reach the journal unless the Worker is stuck.
      const settled = this.request("durable").catch(() => {});
      await Promise.race([settled, new Promise((r) => setTimeout(r, 2000))]);
    }
    this.shutdown();
    // Another engine may take the namespace as soon as this resolves.
    await this.leaseReleased;
  }
}

/** Starts an engine in `options.container`. */
export async function createEngine(options: EngineOptions): Promise<Engine> {
  return EngineCore.create(options);
}
