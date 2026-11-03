import { AltoError } from "./errors.js";
import { intentRecord } from "./intents.js";
import { SnapshotStore } from "./snapshot.js";
import { immutable, frameEntries, sha256Hex, unframeEntries, } from "./storage.js";
import { applyTextStyle, replaceText } from "./text-edit.js";
import { ToolSession } from "./tools.js";
import { Transaction } from "./transaction.js";
/**
 * The open document. Read it with `getSnapshot` and `subscribe`; edit it with
 * `transact` and the named helpers. The same object stays valid while the engine
 * restarts its Worker; `Engine.open`, `createDocument` and `recovery.restore`
 * give a new one.
 */
export class DocumentHandle {
    core;
    store = new SnapshotStore();
    listeners = new Set();
    tool = null;
    savedFolder = null;
    /** @internal */
    constructor(core, state) {
        this.core = core;
        this.store.apply(state);
    }
    /** The current state. Unchanged until the next edit, so it is safe to keep. */
    getSnapshot() {
        return this.store.get();
    }
    /** Calls `listener` after each change with what changed. Returns an unsubscribe function. */
    subscribe(listener) {
        this.listeners.add(listener);
        return () => {
            this.listeners.delete(listener);
        };
    }
    /** The stroke in progress, if any. */
    get activeTool() {
        return this.tool;
    }
    /** The folder this document was opened from or last saved to. */
    get folder() {
        return this.savedFolder?.provider ?? null;
    }
    /** @internal */
    apply(state) {
        if (this.closed)
            return;
        const change = this.store.apply(state);
        if (!change)
            return;
        for (const listener of [...this.listeners])
            listener(change);
    }
    /** @internal The Worker went away; any stroke in progress ends with `error`. */
    abortTool(error) {
        this.tool?.abort(error);
        this.tool = null;
    }
    /** @internal */
    async endTool() {
        if (this.tool?.active)
            await this.tool.end();
    }
    /** @internal */
    flushTool() {
        return this.tool?.flush();
    }
    closed = false;
    /** Requests go through here, so a replaced handle cannot touch the new document. */
    request = ((...args) => this.closed
        ? Promise.reject(new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" }))
        : this.core.requestForDocument(this, ...args));
    /** @internal The engine opened another document; this handle refuses further calls. */
    close() {
        this.closed = true;
        this.abortTool(new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" }));
        this.listeners.clear();
    }
    command(type, payload) {
        return this.edit(async () => {
            this.apply(await this.request(type, payload));
        });
    }
    async edit(fn) {
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
    transact(intent, build) {
        return this.edit(async () => {
            const tx = new Transaction(this.getSnapshot());
            const value = build(tx);
            if (value && typeof value.then === "function")
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
    undo() {
        return this.command("undo", {});
    }
    /** Redoes the last undone edit. */
    redo() {
        return this.command("redo", {});
    }
    /**
     * Resolves once every edit made so far is in the recovery journal. Edits resolve
     * when applied; this rejects with STORAGE if the journal could not be written.
     */
    durable() {
        return this.request("durable").then(() => { });
    }
    /** The hash of the snapshot a save would write. Computed on request, not per edit. */
    snapshotHash() {
        return this.request("snapshotHash", {});
    }
    /** A raster layer's full tile map. */
    nodeTiles(id) {
        return this.request("nodeTiles", { node: id });
    }
    /** Brush and eraser strokes. */
    tools = {
        /** Starts a stroke. Only one stroke runs at a time. */
        begin: (tool, options) => {
            if (this.closed)
                throw new AltoError("CONFLICT", "this document was replaced by another one", { reason: "document-closed" });
            const failure = this.core.storageError();
            if (failure)
                throw failure;
            if (this.tool?.active)
                throw new AltoError("CONFLICT", "a tool is already active", { reason: "tool-active" });
            const session = new ToolSession(this.core.toolHost(this), tool, options);
            this.tool = session;
            return session;
        },
    };
    /** @internal A stroke finished, with the engine's state after it (null if none). */
    toolEnded(session, state) {
        if (this.tool === session)
            this.tool = null;
        if (state)
            this.apply(state);
    }
    textOf(id) {
        const node = this.getSnapshot().nodes.get(id);
        if (!node)
            throw new AltoError("NOT_FOUND", `no node ${id}`, { node: id });
        if (node.type !== "text")
            throw new AltoError("INVALID_ARGUMENT", `node ${id} is not text`);
        return structuredClone(node.content);
    }
    /** Text layer editing and layout queries. Offsets are UTF-16 code units. */
    text = {
        set: (id, content) => this.transact("text.edit", (tx) => tx.setText(id, content)),
        /** Replaces a UTF-16 range of the text in one undoable edit. */
        replace: (id, start, end, insert, intent = "text.input") => this.transact(intent, (tx) => tx.setText(id, replaceText(this.textOf(id), start, end, insert))),
        style: (id, start, end, patch) => this.transact("text.style", (tx) => tx.setText(id, applyTextStyle(this.textOf(id), start, end, patch))),
        setTateChuYoko: (id, start, end, on = true) => this.transact("text.tate-chu-yoko", (tx) => {
            const c = this.textOf(id);
            const rest = (c.tcy || []).filter((r) => r.end <= start || r.start >= end);
            c.tcy = on ? [...rest, { start, end }].sort((a, b) => a.start - b.start) : rest;
            tx.setText(id, c);
        }),
        info: (id) => this.request("textInfo", { node: id }),
        /** UTF-16 offset nearest to a document point. */
        hitTest: (id, x, y) => this.request("textHitTest", { node: id, x, y }),
        caret: (id, offset) => this.request("textCaret", { node: id, offset }),
        /** Highlight quads in document coordinates. */
        selectionRects: (id, start, end) => this.request("textSelection", { node: id, start, end }),
        navigate: (id, offset, motion) => this.request("textNavigate", { node: id, offset, motion }),
    };
    /** Vector paths, vector masks and clipping paths. */
    vectors = {
        set: (id, paths) => this.transact("vector.edit", (tx) => tx.setVector(id, paths)),
        setMask: (id, mask) => this.transact("vector.mask", (tx) => tx.setVectorMask(id, mask)),
        setClipPath: (id, path) => this.transact("vector.clip", (tx) => tx.setClipPath(id, path)),
    };
    /** The document selection (an R8 mask). */
    selection = {
        apply: (spec) => this.command("select", { spec }),
        /** The selection's outline as SVG path data in document coordinates. */
        outline: () => this.request("selectionOutline", {}),
        clear: () => this.command("select", { spec: { kind: "clear" } }),
        invert: () => this.command("select", { spec: { kind: "invert" } }),
        all: () => this.command("select", { spec: { kind: "all" } }),
    };
    /** A layer's pixel mask. */
    masks = {
        fromSelection: (layer) => this.command("setMask", { layer, source: "selection" }),
        addWhite: (layer) => this.command("setMask", { layer, source: "white" }),
        remove: (layer) => this.command("setMask", { layer, source: "remove" }),
    };
    /** A layer's non-destructive effects. */
    effects = {
        /** Replaces a layer's effects. With `useSelection` (default) each is limited to the selection. */
        set: (layer, effects, options = {}) => this.command("setEffects", { layer, effects, useSelection: options.useSelection ?? true }),
    };
    imageCommand(edit) {
        return this.edit(async () => { const r = await this.request("imageEdit", { edit }); this.apply(r.state); return r.result; });
    }
    canvasCommand(edit) {
        return this.edit(async () => { const r = await this.request("canvasEdit", { edit }); this.apply(r.state); return r.result; });
    }
    layerCommand(edit) {
        return this.edit(async () => { const r = await this.request("layerEdit", { edit }); this.apply(r.state); return r.result; });
    }
    historyCommand(command) {
        return this.edit(async () => { const r = await this.request("historyCommand", { command }); this.apply(r.state); return r.result; });
    }
    /** Pixel edits use node-local coordinates; each successful edit is one Undo step. */
    image = {
        importSvg: (svg, options = {}) => this.edit(async () => {
            const snapshot = this.getSnapshot();
            const parent = options.parent ?? snapshot.root;
            const group = snapshot.nodes.get(parent);
            const index = options.index ?? (group?.type === "group" ? group.children.length : 0);
            const r = await this.request("importSvg", { svg, parent, index });
            this.apply(r.state);
            return r.result;
        }),
        crop: (node, bounds) => this.imageCommand({ type: "crop", node, bounds }),
        resize: (node, width, height, options = {}) => this.imageCommand({ type: "resize", node, width, height, filter: options.filter ?? "bilinear" }),
        rotate: (node, quarterTurns) => this.imageCommand({ type: "rotate", node, quarter_turns: quarterTurns }),
        flip: (node, axis) => this.imageCommand({ type: "flip", node, axis }),
        gradient: (node, options) => this.imageCommand({ type: "gradient", node, options }),
        fill: (node, point, color, options = {}) => this.imageCommand({ type: "fill", node, point, color, tolerance: options.tolerance ?? 0, selection: options.selection ?? "current" }),
        /** Copies the layer's own pixels along a stroke. `source` is sampled for the first point. */
        cloneStamp: (node, source, stroke, options = {}) => this.imageCommand({ type: "clone_stamp", node, source, stroke: { hardness: 1, ...stroke }, opacity: options.opacity ?? 1, selection: options.selection ?? "current" }),
        /** Carries the pixels under the tip along a stroke. */
        smudge: (node, stroke, options = {}) => this.imageCommand({ type: "smudge", node, stroke: { hardness: 1, ...stroke }, strength: options.strength ?? 0.5, selection: options.selection ?? "current" }),
        /** Merges a raster into the raster below it. Refused when the result could not look the same. */
        mergeDown: (node) => this.layerCommand({ type: "merge_down", node }),
        /** Bakes a whole-pixel offset into a raster, or flattens a group of rasters. */
        rasterize: (node) => this.layerCommand({ type: "rasterize", node }),
        histogram: (node, range) => this.request("imageHistogram", range ? { node, range } : { node }),
        pickColor: (node, point) => this.request("pickColor", { node, x: point[0], y: point[1] }),
        pickComposite: (point) => this.request("pickComposite", { x: point[0], y: point[1] }),
    };
    /**
     * Whole-document geometry, separate from the layer-local `image` edits. Each is one Undo
     * step that moves the document's root transform, so layer pixels stay as they are.
     */
    canvas = {
        /** The rectangle becomes the canvas. Content outside it is kept, only no longer visible. */
        crop: (rect) => this.canvasCommand({ type: "crop", ...rect }),
        /** Scales the whole document to the new canvas size. */
        resize: (width, height) => this.canvasCommand({ type: "resize", width, height }),
        rotate: (quarterTurns) => this.canvasCommand({ type: "rotate", quarter_turns: quarterTurns }),
        flip: (axis) => this.canvasCommand({ type: "flip", axis }),
    };
    /** Version commits, diffs and restores. */
    versions = {
        gc: (options = {}) => this.gcFolder(options),
        /** Returns a new compact archive. Does not overwrite an existing file. */
        compactArchive: (archive) => this.edit(async () => {
            const bytes = await archive.arrayBuffer();
            const framed = await this.request("compactArchive", { bytes }, [bytes]);
            const entries = unframeEntries(framed);
            const result = entries.get("archive.alto");
            if (!result)
                throw new AltoError("INTERNAL", "missing compact archive");
            const report = JSON.parse(new TextDecoder().decode(entries.get("report.json")));
            return { bytes: result, report };
        }),
        createBranch: (name, options = {}) => this.historyCommand({ type: "create_branch", name, from: options.from ?? null, checkout: options.checkout ?? false }).then(() => { }),
        checkout: (name) => this.historyCommand({ type: "checkout", name }).then(() => { }),
        detach: (hash) => this.historyCommand({ type: "detach", hash }).then(() => { }),
        deleteBranch: (name, options = {}) => this.historyCommand({ type: "delete_branch", name, force: options.force ?? false }).then(() => { }),
        setBranchProtection: (name, protectedState) => this.historyCommand({ type: "protect_branch", name, protected: protectedState }).then(() => { }),
        listBranches: () => this.request("listBranches"),
        createTag: (name, hash) => this.historyCommand({ type: "create_tag", name, hash }).then(() => { }),
        deleteTag: (name) => this.historyCommand({ type: "delete_tag", name }).then(() => { }),
        pin: (hash) => this.historyCommand({ type: "pin", hash, pin: true }).then(() => { }),
        unpin: (hash) => this.historyCommand({ type: "pin", hash, pin: false }).then(() => { }),
        prepareMerge: (name) => this.edit(() => this.request("prepareMerge", { name })),
        cancelMerge: () => this.edit(() => this.request("cancelMerge").then(() => { })),
        finishMerge: (token, resolutions, options = {}) => this.historyCommand({ type: "finish_merge", token, resolutions, message: options.message ?? "Merge", author: options.author ?? null, created_at: Date.now() }).then((r) => {
            if (!r)
                throw new AltoError("INTERNAL", "merge returned no result");
            return r;
        }),
        /** Records the current document as a version. Fails when nothing changed unless allowEmpty. */
        commit: (message, options = {}) => this.edit(async () => {
            const r = await this.request("commit", {
                message,
                author: options.author || "",
                allowEmpty: !!options.allowEmpty,
                time: Date.now(),
            });
            this.apply(r.state);
            return r.commit;
        }),
        log: (options = {}) => this.request("versionLog", { dag: options.mode === "dag", allBranches: options.allBranches ?? false }),
        /** Semantic, layer-level diff. Use "working" for the current document. */
        diff: (from, to = "working") => this.request("versionDiff", { from, to }),
        status: () => this.getSnapshot().versions,
        /** Restores a version as one undoable edit; history is kept. */
        restore: (hash) => this.command("restoreVersion", { hash }),
        /** Drops versions that cannot be reached from the head. Returns how many. */
        prune: () => this.edit(async () => {
            const state = await this.request("pruneVersions");
            this.apply(state);
            return state.removed ?? 0;
        }),
    };
    /**
     * The document, or a region of it, as a PNG or WebP image: the pixels the canvas
     * shows, at `scale` (greater than 0, at most 1). Allowed while storage has failed.
     */
    async exportImage(options = {}) {
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
    async save(target) {
        await this.endTool();
        this.core.emit("saveState", "saving");
        let token;
        try {
            const saved = await this.request("save");
            token = saved.token;
            await target.write(new Blob([saved.bytes]).stream());
            this.apply(await this.request("saved", { token }));
            token = undefined;
            this.core.emit("saveState", "saved");
        }
        catch (e) {
            this.releaseSave(token);
            this.core.emit("saveState", "error");
            throw e;
        }
    }
    gcFolder(options) {
        return this.edit(async () => {
            const owner = this.savedFolder?.provider;
            if (!owner)
                throw new AltoError("UNSUPPORTED", "save or open a folder before collecting it");
            const dryRun = options.dryRun ?? true;
            const capabilities = owner.gcCapabilities;
            if (!dryRun && (typeof owner.exclusive !== "function" || capabilities?.exclusive !== true || capabilities.atomicManifest !== true || capabilities.deleteObjects !== true))
                throw new AltoError("UNSUPPORTED", "storage does not guarantee exclusive GC and atomic manifest replacement");
            const run = async (storage) => {
                if (!dryRun && typeof storage.deleteObjects !== "function")
                    throw new AltoError("UNSUPPORTED", "exclusive storage view does not support deletion");
                const readAll = async () => {
                    const manifest = await storage.readRef("manifest.json");
                    if (!manifest)
                        throw new AltoError("CORRUPT_DATA", "missing manifest");
                    const entries = new Map([["manifest.json", manifest]]);
                    for (const prefix of ["objects/", "snapshots/", "commits/"])
                        for (const path of await storage.list(prefix)) {
                            const bytes = await storage.getObject(path);
                            if (!bytes)
                                throw new AltoError("CONFLICT", "object changed during GC");
                            entries.set(path, bytes);
                        }
                    const after = await storage.readRef("manifest.json");
                    if (!after || await sha256Hex(after) !== await sha256Hex(manifest))
                        throw new AltoError("CONFLICT", "manifest changed during GC");
                    return entries;
                };
                const stored = await readAll();
                const framed = frameEntries(stored);
                const output = await this.request("gcPlan", { bytes: framed.buffer, dryRun }, [framed.buffer]);
                const entries = unframeEntries(output);
                const plan = JSON.parse(new TextDecoder().decode(entries.get(".gc-plan.json")));
                entries.delete(".gc-plan.json");
                if (options.expectedBaseline !== undefined && options.expectedBaseline !== plan.report.baseline)
                    throw new AltoError("CONFLICT", "GC preview expired");
                if (dryRun)
                    return plan.report;
                const manifest = entries.get("manifest.json");
                await storage.writeRef(".alto-gc.json", new TextEncoder().encode(JSON.stringify({ phase: "prepared", baseline: plan.report.baseline, remove: plan.remove })));
                await storage.putObjects([...entries].filter(([path]) => immutable(path)).map(([path, data]) => ({ path, data })));
                const adopt = async () => {
                    const r = await this.request("historyCommand", { command: { type: "adopt_gc", manifest: new TextDecoder().decode(manifest) } });
                    this.apply(r.state);
                    this.savedFolder = { provider: owner, manifest: await sha256Hex(manifest) };
                };
                try {
                    await storage.writeRef("manifest.json", manifest, plan.report.baseline);
                }
                catch (error) {
                    const after = await storage.readRef("manifest.json");
                    if (after && await sha256Hex(after) === await sha256Hex(manifest))
                        await adopt();
                    throw error;
                }
                await adopt();
                let removed = 0;
                try {
                    for (const path of plan.remove) {
                        await storage.deleteObjects([path]);
                        removed++;
                    }
                }
                catch (error) {
                    throw new AltoError("STORAGE", `GC stopped after ${removed} deletions: ${String(error)}`);
                }
                const check = frameEntries(await readAll());
                await this.request("gcPlan", { bytes: check.buffer, dryRun: true }, [check.buffer]);
                for (const [path, data] of entries)
                    if (!immutable(path) && path !== "manifest.json") {
                        try {
                            await storage.writeRef(path, data);
                        }
                        catch {
                            plan.report.warnings.push(`比較用ファイル ${path} を更新できませんでした。正本は保持されています。`);
                        }
                    }
                await storage.writeRef(".alto-gc.json", new TextEncoder().encode(JSON.stringify({ phase: "complete", baseline: plan.report.baseline })));
                return plan.report;
            };
            return owner.exclusive ? owner.exclusive(run) : run(owner);
        });
    }
    /**
     * Saves the folder form. Content-addressed files are written once; manifest.json
     * last. A folder holding another document is refused unless `overwrite`.
     */
    async saveFolder(provider, options = {}) {
        await this.endTool();
        return provider.exclusive ? provider.exclusive((storage) => this.saveFolderLocked(provider, storage, options)) : this.saveFolderLocked(provider, provider, options);
    }
    async saveFolderLocked(owner, provider, options) {
        this.core.emit("saveState", "saving");
        let token;
        try {
            const current = await provider.readRef("manifest.json");
            const currentHash = current ? await sha256Hex(current) : "";
            const base = this.savedFolder?.provider === owner ? this.savedFolder.manifest : undefined;
            if (current && base === undefined && !options.overwrite)
                throw new AltoError("CONFLICT", "the folder holds another document", { reason: "folder-occupied" });
            if (base !== undefined && base !== currentHash)
                throw new AltoError("CONFLICT", "manifest.json changed elsewhere", { reason: "folder-changed" });
            const saved = await this.request("folderEntries");
            token = saved.token;
            const entries = unframeEntries(new Uint8Array(saved.bytes));
            const manifest = entries.get("manifest.json");
            if (!manifest)
                throw new AltoError("INTERNAL", "manifest missing");
            const objects = [...entries].filter(([p]) => immutable(p)).map(([path, data]) => ({ path, data }));
            const report = await provider.putObjects(objects);
            for (const [p, d] of entries)
                if (!immutable(p) && p !== "manifest.json" && p !== "document.json")
                    await provider.writeRef(p, d);
            await provider.writeRef("manifest.json", manifest, base ?? currentHash);
            const check = await provider.readRef("manifest.json");
            if (!check || check.length !== manifest.length || check.some((b, i) => b !== manifest[i]))
                throw new AltoError("STORAGE", "manifest.json did not read back as written");
            const warnings = [];
            const mirror = entries.get("document.json");
            if (mirror) {
                try {
                    await provider.writeRef("document.json", mirror);
                }
                catch {
                    warnings.push("比較用の文書コピーを更新できませんでした。正本の保存は完了しています。");
                }
            }
            this.savedFolder = { provider: owner, manifest: await sha256Hex(manifest) };
            this.apply(await this.request("saved", { token }));
            token = undefined;
            this.core.emit("saveState", "saved");
            return { ...report, warnings };
        }
        catch (e) {
            this.releaseSave(token);
            this.core.emit("saveState", "error");
            throw e;
        }
    }
    /** @internal */
    setFolder(provider, manifest) {
        this.savedFolder = { provider, manifest };
    }
    /** A cancelled or failed save no longer needs the Worker's copy of the package. */
    releaseSave(token) {
        if (token !== undefined)
            this.request("releaseSave", { token }).catch(() => { });
    }
}
