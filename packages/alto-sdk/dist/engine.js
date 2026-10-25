import { DocumentHandle } from "./document.js";
import { AltoError, fromWorkerError } from "./errors.js";
import { Emitter } from "./events.js";
import { PROTOCOL } from "./protocol.js";
import { frameEntries, sha256Hex } from "./storage.js";
import { ViewportControl } from "./viewport.js";
const SDK_VERSION = "0.5.0";
const readableAfterStorageFailure = new Set([
    "emergencyExport", "save", "view", "waitFrame", "readTile", "readTileLevel",
    "selectionOutline", "textInfo", "textHitTest", "textCaret",
    "textSelection", "textNavigate", "versionLog", "versionDiff",
    "vectorStats", "vectorPathSvg", "releaseSave", "snapshotHash", "nodeTiles", "exportImage",
]);
const NAMESPACE = /^[A-Za-z0-9._/-]{1,128}$/;
/** @internal The engine. Public callers see it through `Engine`. */
export class EngineCore extends Emitter {
    options;
    element;
    document = null;
    viewport;
    /** @internal */
    diagnostics = new Emitter();
    /** @internal */
    namespace;
    worker = null;
    sequence = 1;
    pending = new Map();
    mutation = Promise.resolve();
    nextDocumentEpoch = 0;
    documentEpoch = 0;
    documentEpochs = new WeakMap();
    raf = 0;
    disposed = false;
    pixelRatio;
    state = { state: "starting", error: null };
    workerGeneration = 0;
    restartPromise = null;
    releaseLease;
    /** Settles once the namespace lock is released, after `releaseLease`. */
    leaseReleased = Promise.resolve();
    lastWorkerPanic = null;
    recoveryWaiting = false;
    resizeObserver;
    ratioQuery = null;
    onRatioChange = () => this.watchPixelRatio();
    /**
     * Leaving the page ends the engine. A page kept in the back/forward cache only
     * pauses: a journaled engine gives up its namespace (another tab may open the
     * document meanwhile) and restarts from the journal when the page returns. An
     * engine without a journal keeps its Worker, which the browser freezes with the page.
     */
    onPageHide = (event) => {
        if (!event.persisted)
            this.shutdown();
        else if (this.journaled)
            this.suspend();
    };
    onPageShow = (event) => {
        if (event.persisted && this.suspended)
            void this.resume().catch((e) => this.emit("error", e));
    };
    suspended = false;
    /** @internal Set by `assets.baseURL` or a test; otherwise the bundled Worker is used. */
    workerURL;
    constructor(options) {
        super();
        this.options = options;
        this.namespace = options.storage?.namespace ?? "default";
        this.pixelRatio = this.currentRatio();
        this.workerURL = options.assets?.baseURL
            ? new URL("worker.js", new URL(options.assets.baseURL, location.href))
            : null;
        this.element = this.makeCanvas();
        options.container.appendChild(this.element);
        this.viewport = new ViewportControl({
            rect: () => this.element.getBoundingClientRect(),
            pixelRatio: () => this.pixelRatio,
            documentSize: () => {
                const meta = this.document?.getSnapshot().meta;
                return meta ? [meta.width, meta.height] : null;
            },
            send: (v) => this.stream({
                t: "viewport",
                version: v.version,
                center: v.center,
                zoom: v.deviceZoom,
                rotation: v.rotation,
            }),
            changed: (v) => this.emit("viewport", v),
        });
        this.resizeObserver = new ResizeObserver(() => this.resize());
        this.resizeObserver.observe(options.container);
        this.watchPixelRatio();
        addEventListener("pagehide", this.onPageHide);
        addEventListener("pageshow", this.onPageShow);
    }
    /** @internal */
    static async create(options) {
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
            if (e.journaled)
                await e.acquireLease();
            await e.start();
            e.setStatus("ready", null);
            e.startLoop();
            return e;
        }
        catch (error) {
            await e.dispose();
            throw error;
        }
    }
    get status() {
        return this.state;
    }
    /** @internal */
    get journaled() {
        return this.options.storage?.recovery !== false;
    }
    /** @internal */
    get pixelRatioValue() {
        return this.pixelRatio;
    }
    /** @internal */
    storageError() {
        return this.state.state === "storage-failed" ? this.state.error : null;
    }
    setStatus(state, error) {
        if (this.state.state === state && this.state.error === error)
            return;
        this.state = Object.freeze({ state, error });
        this.emit("status", this.state);
    }
    currentRatio() {
        return Math.min(devicePixelRatio || 1, this.options.maxPixelRatio ?? 2);
    }
    makeCanvas() {
        const c = document.createElement("canvas");
        c.style.cssText = "display:block;width:100%;height:100%;touch-action:none";
        return c;
    }
    watchPixelRatio() {
        this.ratioQuery?.removeEventListener("change", this.onRatioChange);
        if (this.disposed)
            return;
        this.ratioQuery = matchMedia(`(resolution: ${devicePixelRatio || 1}dppx)`);
        this.ratioQuery.addEventListener("change", this.onRatioChange);
        const ratio = this.currentRatio();
        if (ratio !== this.pixelRatio) {
            this.pixelRatio = ratio;
            this.resize();
            this.viewport.publish();
        }
    }
    async acquireLease() {
        if (!navigator.locks)
            throw new AltoError("CONFLICT", "this browser has no Web Locks, so a journal cannot be held exclusively", {
                reason: "another-tab",
            });
        let release;
        const hold = new Promise((resolve) => {
            release = resolve;
        });
        await new Promise((resolve, reject) => {
            this.leaseReleased = navigator.locks
                .request(`alto-engine:${this.namespace}`, { ifAvailable: true }, async (lock) => {
                if (!lock) {
                    reject(new AltoError("CONFLICT", `storage namespace ${this.namespace} is in use by another engine`, {
                        reason: "namespace-in-use",
                        namespace: this.namespace,
                    }));
                    return;
                }
                this.releaseLease = release;
                resolve();
                await hold;
            })
                .catch(reject)
                .catch(() => { });
        });
    }
    async start() {
        // Written out literally so bundlers (Vite, webpack) see the Worker and bundle it.
        const worker = this.workerURL
            ? new Worker(this.workerURL, { type: "module" })
            : new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
        this.worker = worker;
        const generation = ++this.workerGeneration;
        worker.onmessage = ({ data }) => {
            if (generation !== this.workerGeneration || this.disposed)
                return;
            this.onWorkerMessage(data);
        };
        worker.onerror = (event) => {
            if (generation !== this.workerGeneration || this.disposed)
                return;
            // The Worker reports a Rust panic just before its error reaches here.
            const panic = this.lastWorkerPanic;
            this.lastWorkerPanic = null;
            const running = this.state.state === "ready" || this.state.state === "storage-failed";
            const detail = (event.message || "") + (panic ? " (panic: " + panic + ")" : "");
            const e = new AltoError(running ? "WORKER_CRASHED" : "WORKER_UNAVAILABLE", detail || (running ? "the Worker stopped" : "the Worker could not be loaded"));
            this.diagnostics.emit("workerDiagnostic", {
                state: this.state.state,
                workerURL: String(this.workerURL ?? new URL("./worker.js", import.meta.url)),
                message: event.message || null,
                file: event.filename || null,
                line: event.lineno || null,
            });
            this.rejectPending(e);
            if (running)
                void this.restart().catch((failure) => this.emit("error", failure));
        };
        const offscreen = this.element.transferControlToOffscreen();
        const boot = (await this.requestCore("init", {
            protocol: PROTOCOL,
            sdk: SDK_VERSION,
            canvas: offscreen,
            budget: this.options.gpu?.memoryBudgetMB ?? 512,
            namespace: this.namespace,
            journal: this.journaled,
        }, [offscreen], true, 30000));
        if (this.worker !== worker || this.disposed)
            throw new AltoError("WORKER_UNAVAILABLE", "the Worker start was interrupted");
        if (boot.protocol !== PROTOCOL)
            throw new AltoError("CONFLICT", "SDK/WASM protocol mismatch");
        this.recoveryWaiting = !!boot.recovery.available;
        if (this.recoveryWaiting)
            this.emit("recoveryAvailable", { records: boot.recovery.records });
        this.resize();
    }
    onWorkerMessage(data) {
        if (data.t === "reply") {
            const p = this.pending.get(data.id);
            if (!p)
                return;
            this.pending.delete(data.id);
            if (p.timer)
                clearTimeout(p.timer);
            if (data.ok)
                p.resolve(data.value);
            else
                p.reject(fromWorkerError(data.error));
        }
        else if (data.t === "patch") {
            this.document?.apply(data.value);
        }
        else if (data.t === "storageFailure") {
            if (this.state.state !== "storage-failed") {
                const error = new AltoError("STORAGE", data.value.message);
                this.document?.abortTool(error);
                this.setStatus("storage-failed", error);
            }
        }
        else if (data.t === "frame") {
            this.diagnostics.emit("frame", data.value);
        }
        else if (data.t === "diagnostic") {
            const value = data.value;
            if (value?.panic)
                this.lastWorkerPanic = String(value.panic);
            this.diagnostics.emit("diagnostic", value);
        }
        else if (data.t === "gpuLost") {
            this.document?.abortTool(new AltoError("GPU_LOST", "the GPU device was lost during the stroke"));
            this.diagnostics.emit("gpuLost", null);
        }
        else if (data.t === "gpuRestored") {
            this.diagnostics.emit("gpuRestored", null);
        }
        else if (data.t === "error") {
            this.emit("error", fromWorkerError(data.value));
        }
    }
    startLoop() {
        const generation = this.workerGeneration;
        const loop = () => {
            if (this.disposed || generation !== this.workerGeneration)
                return;
            if (this.state.state !== "ready" && this.state.state !== "storage-failed")
                return;
            void this.document?.flushTool()?.catch((e) => this.emit("error", e));
            this.stream({ t: "tick", time: performance.now() });
            this.raf = requestAnimationFrame(loop);
        };
        this.raf = requestAnimationFrame(loop);
    }
    /** @internal A Worker request, typed by `protocol.ts`. Requests without arguments may omit them. */
    request(t, ...rest) {
        const [args, transfer = []] = rest;
        return this.requestCore(t, (args ?? {}), transfer, false);
    }
    /** @internal Any request by name, for the testing entry point. */
    requestRaw(t, payload = {}) {
        return this.requestCore(t, payload, [], false);
    }
    /** @internal Binds a request to this handle, including across a Worker round trip. */
    requestForDocument(doc, t, ...rest) {
        const epoch = this.documentEpochs.get(doc);
        if (doc !== this.document || epoch === undefined)
            return Promise.reject(new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" }));
        const [args, transfer = []] = rest;
        return this.requestCore(t, (args ?? {}), transfer, false, 0, { documentEpoch: epoch });
    }
    requestCore(t, payload, transfer, internal, timeoutMs = 0, scope = {}) {
        if (this.disposed)
            return Promise.reject(new AltoError("WORKER_UNAVAILABLE", "the engine was disposed"));
        const running = this.state.state === "ready" || this.state.state === "storage-failed";
        if (!internal && !running)
            return Promise.reject(this.state.error || new AltoError("WORKER_UNAVAILABLE", "the engine is restarting"));
        const failure = this.storageError();
        if (failure && !internal && !readableAfterStorageFailure.has(t) && !(t === "open" && failure.code === "CORRUPT_DATA"))
            return Promise.reject(failure);
        const worker = this.worker;
        if (!worker)
            return Promise.reject(new AltoError("WORKER_UNAVAILABLE", "no Worker"));
        const id = this.sequence++;
        return new Promise((resolve, reject) => {
            const timer = timeoutMs
                ? setTimeout(() => {
                    this.pending.delete(id);
                    reject(new AltoError("WORKER_TIMEOUT", `no reply to ${t}`));
                }, timeoutMs)
                : undefined;
            this.pending.set(id, { resolve, reject, timer });
            // Request fields come last so a payload can never replace the reply id.
            try {
                worker.postMessage({ ...payload, ...scope, t, id }, transfer);
            }
            catch (error) {
                if (timer)
                    clearTimeout(timer);
                this.pending.delete(id);
                reject(error);
            }
        });
    }
    rejectPending(error) {
        for (const p of this.pending.values()) {
            if (p.timer)
                clearTimeout(p.timer);
            p.reject(error);
        }
        this.pending.clear();
    }
    /** @internal */
    stream(message, transfer = []) {
        if (this.disposed || this.state.state === "failed" || !this.worker)
            return;
        this.worker.postMessage(message, transfer);
    }
    /** @internal Runs edits one at a time. */
    mutate(fn) {
        return this.serialize(async () => {
            if (this.state.state === "failed" && this.state.error)
                throw this.state.error;
            const failure = this.storageError();
            if (failure)
                throw failure;
            return fn();
        });
    }
    /** Document replacement and recovery decisions share the edit order. */
    serialize(fn) {
        const p = this.mutation.then(fn);
        this.mutation = p.catch(() => { });
        return p;
    }
    /** @internal */
    toolHost(doc) {
        return {
            request: ((...args) => this.requestForDocument(doc, ...args)),
            stream: (m, transfer) => {
                if (doc !== this.document)
                    return;
                this.stream(m.t === "input" ? { ...m, documentEpoch: this.documentEpochs.get(doc) } : m, transfer);
            },
            screenToDocument: (x, y) => this.viewport.screenToDocument(x, y),
            storageError: () => this.storageError(),
            ended: (session, state) => doc.toolEnded(session, state),
            reportError: (e) => this.emit("error", e instanceof AltoError ? e : new AltoError("INTERNAL", String(e))),
        };
    }
    resize() {
        const r = this.options.container.getBoundingClientRect();
        // A hidden container is 0×0; render 1×1 until it is shown.
        this.stream({
            t: "resize",
            width: Math.max(1, Math.round(r.width * this.pixelRatio)),
            height: Math.max(1, Math.round(r.height * this.pixelRatio)),
        });
    }
    adopt(state, fit, epoch) {
        this.document?.close();
        this.document = new DocumentHandle(this, state);
        this.documentEpoch = epoch;
        this.documentEpochs.set(this.document, epoch);
        this.emit("document", this.document);
        if (fit)
            this.viewport.fit();
        else
            this.viewport.publish();
        return this.document;
    }
    /** A waiting journal would be overwritten by the new document's first checkpoint. */
    refuseWhileRecoveryWaits() {
        if (this.recoveryWaiting)
            throw new AltoError("CONFLICT", "a recovery journal is waiting: restore or discard it first", {
                reason: "recovery-pending",
            });
    }
    replaceDocument(load) {
        return this.serialize(async () => {
            this.refuseWhileRecoveryWaits();
            const reopening = this.storageError()?.code === "CORRUPT_DATA";
            const epoch = ++this.nextDocumentEpoch;
            const state = await load(epoch);
            const doc = this.adopt(state, true, epoch);
            if (reopening)
                this.setStatus("ready", null);
            return doc;
        });
    }
    async requestReplacement(t, args, epoch, transfer = []) {
        // Finish the old stroke before the Worker replaces its document, not after.
        await this.document?.endTool();
        return this.requestCore(t, args, transfer, false, 0, { nextDocumentEpoch: epoch });
    }
    async createDocument(meta) {
        return this.replaceDocument((epoch) => this.requestReplacement("createDocument", meta, epoch));
    }
    async open(source) {
        return this.replaceDocument(async (epoch) => {
            const blob = source instanceof Blob ? source : await source.read();
            const bytes = await blob.arrayBuffer();
            return this.requestReplacement("open", { bytes }, epoch, [bytes]);
        });
    }
    async openFolder(provider) {
        let manifestHash = "";
        const doc = await this.replaceDocument(async (epoch) => {
            const manifest = await provider.readRef("manifest.json");
            if (!manifest)
                throw new AltoError("NOT_FOUND", "the folder has no manifest.json");
            manifestHash = await sha256Hex(manifest);
            const entries = new Map([["manifest.json", manifest]]);
            for (const p of [...(await provider.list("snapshots/")), ...(await provider.list("commits/"))]) {
                const d = await provider.getObject(p);
                if (d)
                    entries.set(p, d);
            }
            const meta = frameEntries(entries);
            const plan = await this.request("folderPlan", { bytes: meta.buffer }, [meta.buffer]);
            for (const p of plan) {
                const d = await provider.getObject(p);
                if (d)
                    entries.set(p, d);
            }
            const all = frameEntries(entries);
            return this.requestReplacement("openFolder", { bytes: all.buffer }, epoch, [all.buffer]);
        });
        doc.setFolder(provider, manifestHash);
        return doc;
    }
    recovery = this.makeRecovery();
    makeRecovery() {
        const self = this;
        return {
            get available() {
                return self.recoveryWaiting;
            },
            restore: () => self.restoreRecovery(),
            discard: () => self.discardRecovery(),
            exportEmergency: () => self.exportEmergency(),
            recoverAfterStorageFailure: () => self.recoverAfterStorageFailure(),
        };
    }
    restoreRecovery = () => this.serialize(async () => {
        if (!this.journaled)
            throw new AltoError("NOT_FOUND", "the engine keeps no recovery journal");
        this.requireWaitingRecovery();
        const epoch = ++this.nextDocumentEpoch;
        const state = await this.requestReplacement("recover", {}, epoch);
        this.recoveryWaiting = false;
        return this.adopt(state, true, epoch);
    });
    discardRecovery = () => this.serialize(async () => {
        this.requireWaitingRecovery();
        await this.request("discardRecovery");
        this.recoveryWaiting = false;
    });
    requireWaitingRecovery() {
        if (!this.recoveryWaiting || this.document)
            throw new AltoError("CONFLICT", "there is no waiting recovery journal", { reason: "no-recovery" });
    }
    exportEmergency = async () => new Blob([(await this.request("emergencyExport"))]);
    recoverAfterStorageFailure = async () => {
        const failure = this.storageError();
        if (!failure)
            throw new AltoError("CONFLICT", "storage has not failed", { reason: "not-failed" });
        const snapshot = this.document?.getSnapshot();
        const before = snapshot && { revision: snapshot.revision, stateHash: snapshot.stateHash };
        await this.restart();
        if (!this.document)
            throw new AltoError("NOT_FOUND", "no journal to recover from");
        const after = this.document.getSnapshot();
        return {
            document: this.document,
            beforeRevision: before?.revision ?? null,
            recoveredRevision: after.revision,
            matchesDisplay: before ? before.stateHash === after.stateHash : null,
        };
    };
    assets = {
        importImage: async (file) => {
            const bitmap = await createImageBitmap(file);
            const c = new OffscreenCanvas(bitmap.width, bitmap.height);
            const ctx = c.getContext("2d", { willReadFrequently: true });
            if (!ctx)
                throw new AltoError("INTERNAL", "no 2D context to decode the image");
            ctx.drawImage(bitmap, 0, 0);
            bitmap.close();
            const pixels = ctx.getImageData(0, 0, c.width, c.height).data;
            const encoded = await file.arrayBuffer();
            const asset = await this.request("import", { width: c.width, height: c.height, pixels: pixels.buffer, encoded, mime: file.type || "image/png" }, [pixels.buffer, encoded]);
            return { ...asset, id: asset.record.id };
        },
        importFont: async (file, options = {}) => {
            const bytes = await file.arrayBuffer();
            const font = await this.request("importFont", { bytes, faceIndex: options.faceIndex ?? 0 }, [bytes]);
            return { ...font, id: font.record.id };
        },
        releaseImport: (asset) => {
            const record = asset.record;
            return this.request("releaseImport", { hash: record.hash });
        },
    };
    vectors = {
        parseSvg: (svg) => this.request("parseVectorPath", { svg }),
        toSvg: (data) => this.request("vectorPathSvg", { data }),
    };
    retry() {
        if (this.state.state !== "failed")
            return Promise.reject(new AltoError("CONFLICT", "the engine has not failed", { reason: "not-failed" }));
        // A failed return from the back/forward cache left the namespace to someone else.
        const run = async () => {
            if (this.journaled && !this.releaseLease)
                await this.acquireLease();
            await this.restart();
        };
        const previous = this.restartPromise;
        return previous ? previous.catch(() => { }).then(run) : run();
    }
    /** @internal Stops the Worker and starts a new one with the confirmed journal. */
    restart() {
        if (this.disposed)
            return Promise.reject(new AltoError("WORKER_UNAVAILABLE", "the engine was disposed"));
        if (this.restartPromise)
            return this.restartPromise;
        const task = Promise.resolve().then(() => this.performRestart());
        this.restartPromise = task;
        // After the promise is stored: a listener that asks for a restart shares it.
        this.setStatus("restarting", null);
        const clear = () => {
            if (this.restartPromise === task)
                this.restartPromise = null;
        };
        void task.then(clear, clear);
        return task;
    }
    async performRestart() {
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
            if (this.disposed)
                throw new AltoError("WORKER_UNAVAILABLE", "the engine was disposed");
            if (this.document || this.recoveryWaiting) {
                const epoch = this.document ? this.documentEpoch : ++this.nextDocumentEpoch;
                const state = await this.requestCore("recover", {}, [], true, 30000, { nextDocumentEpoch: epoch });
                if (this.disposed)
                    throw new AltoError("WORKER_UNAVAILABLE", "the engine was disposed");
                this.recoveryWaiting = false;
                // The same handle carries on; the host's references stay valid.
                if (this.document) {
                    this.document.apply(state);
                    this.viewport.publish();
                }
                else
                    this.adopt(state, true, epoch);
            }
            this.setStatus("ready", null);
            this.startLoop();
        }
        catch (cause) {
            if (this.disposed)
                throw cause;
            const error = cause instanceof AltoError
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
            }
            else {
                this.worker?.terminate();
                this.setStatus("failed", error);
            }
            throw error;
        }
    }
    /** The page went into the back/forward cache. */
    suspend() {
        if (this.disposed || this.suspended)
            return;
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
    async resume() {
        this.suspended = false;
        await this.leaseReleased;
        try {
            await this.acquireLease();
        }
        catch (error) {
            this.setStatus("failed", error instanceof AltoError ? error : new AltoError("CONFLICT", String(error)));
            throw error;
        }
        await this.restart();
    }
    shutdown() {
        if (this.disposed)
            return;
        this.disposed = true;
        ++this.workerGeneration;
        cancelAnimationFrame(this.raf);
        this.worker?.terminate();
        this.worker = null;
        this.releaseLease?.();
        this.releaseLease = undefined;
        this.resizeObserver.disconnect();
        this.ratioQuery?.removeEventListener("change", this.onRatioChange);
        removeEventListener("pagehide", this.onPageHide);
        removeEventListener("pageshow", this.onPageShow);
        this.document?.abortTool(new AltoError("WORKER_UNAVAILABLE", "the engine was disposed"));
        this.rejectPending(new AltoError("WORKER_UNAVAILABLE", "the engine was disposed"));
        this.element.remove();
        this.setStatus("disposed", null);
    }
    async dispose() {
        if (this.disposed)
            return;
        if (this.worker && this.document && this.state.state === "ready") {
            // Edits already applied reach the journal unless the Worker is stuck.
            const settled = this.request("durable").catch(() => { });
            await Promise.race([settled, new Promise((r) => setTimeout(r, 2000))]);
        }
        this.shutdown();
        // Another engine may take the namespace as soon as this resolves.
        await this.leaseReleased;
    }
}
/** Starts an engine in `options.container`. */
export async function createEngine(options) {
    return EngineCore.create(options);
}
