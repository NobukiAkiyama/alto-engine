export async function sha256Hex(data) {
    const d = await crypto.subtle.digest("SHA-256", data);
    return Array.from(new Uint8Array(d), (b) => b.toString(16).padStart(2, "0")).join("");
}
const immutable = (p) => p.startsWith("objects/") || p.startsWith("snapshots/") || p.startsWith("commits/");
function validPath(p) {
    return !!p && !p.startsWith("/") && p.split("/").every((s) => s && s !== "." && s !== "..");
}
export class MemoryObjectProvider {
    kind = "memory";
    files = new Map();
    async getObject(path) { return this.files.get(path)?.slice() ?? null; }
    async putObjects(entries) {
        let written = 0, skipped = 0;
        for (const e of entries) {
            if (!validPath(e.path))
                throw new AltoError("INVALID_ARGUMENT", "不正なパス: " + e.path);
            if (this.files.has(e.path))
                skipped++;
            else {
                this.files.set(e.path, e.data.slice());
                written++;
            }
        }
        return { written, skipped };
    }
    async list(prefix) { return [...this.files.keys()].filter((k) => k.startsWith(prefix)).sort(); }
    async readRef(name) { return this.getObject(name); }
    async writeRef(name, data, expectHash) {
        if (expectHash !== undefined) {
            const cur = this.files.get(name);
            if ((cur ? await sha256Hex(cur) : "") !== expectHash)
                throw new AltoError("CONFLICT", name + " が他で変更されています");
        }
        this.files.set(name, data.slice());
    }
}
/** A directory from the File System Access API or OPFS. Writes replace atomically on close. */
export class FileSystemDirectoryProvider {
    handle;
    kind = "fs-access-dir";
    constructor(handle) {
        this.handle = handle;
    }
    async dir(parts, create) {
        let d = this.handle;
        for (const p of parts)
            d = await d.getDirectoryHandle(p, { create });
        return d;
    }
    async file(path, create) {
        if (!validPath(path))
            throw new AltoError("INVALID_ARGUMENT", "不正なパス: " + path);
        const parts = path.split("/");
        const name = parts.pop();
        return (await this.dir(parts, create)).getFileHandle(name, { create });
    }
    async getObject(path) {
        try {
            const f = await (await this.file(path, false)).getFile();
            return new Uint8Array(await f.arrayBuffer());
        }
        catch (e) {
            if (e?.name === "NotFoundError" || e?.name === "TypeError")
                return null;
            throw new AltoError("STORAGE", String(e));
        }
    }
    async write(path, data) {
        try {
            const w = await (await this.file(path, true)).createWritable();
            try {
                await w.write(data);
                await w.close();
            }
            catch (e) {
                await w.abort();
                throw e;
            }
        }
        catch (e) {
            throw e instanceof AltoError ? e : new AltoError("STORAGE", String(e));
        }
    }
    async putObjects(entries) {
        let written = 0, skipped = 0;
        for (const e of entries) {
            if (await this.exists(e.path))
                skipped++;
            else {
                await this.write(e.path, e.data);
                written++;
            }
        }
        return { written, skipped };
    }
    async exists(path) {
        try {
            await this.file(path, false);
            return true;
        }
        catch {
            return false;
        }
    }
    async list(prefix) {
        const out = [];
        const parts = prefix.split("/").filter(Boolean);
        let root;
        try {
            root = await this.dir(parts, false);
        }
        catch {
            return out;
        }
        const walk = async (d, base) => {
            for await (const [name, h] of d.entries()) {
                if (name.startsWith("."))
                    continue;
                if (h.kind === "directory")
                    await walk(h, base + name + "/");
                else
                    out.push(base + name);
            }
        };
        await walk(root, parts.length ? parts.join("/") + "/" : "");
        return out.sort();
    }
    async readRef(name) { return this.getObject(name); }
    async writeRef(name, data, expectHash) {
        if (expectHash !== undefined) {
            const cur = await this.getObject(name);
            if ((cur ? await sha256Hex(cur) : "") !== expectHash)
                throw new AltoError("CONFLICT", name + " が他で変更されています");
        }
        await this.write(name, data);
    }
}
/** Worker <-> main framing for package entries. */
export function frameEntries(entries) {
    const enc = new TextEncoder();
    const names = [...entries.keys()].map((k) => enc.encode(k));
    let size = 4;
    let i = 0;
    for (const v of entries.values())
        size += 8 + names[i++].length + v.length;
    const out = new Uint8Array(size);
    const view = new DataView(out.buffer);
    view.setUint32(0, entries.size, true);
    let p = 4;
    i = 0;
    for (const v of entries.values()) {
        const n = names[i++];
        view.setUint32(p, n.length, true);
        out.set(n, p + 4);
        p += 4 + n.length;
        view.setUint32(p, v.length, true);
        out.set(v, p + 4);
        p += 4 + v.length;
    }
    return out;
}
export function unframeEntries(b) {
    const view = new DataView(b.buffer, b.byteOffset, b.byteLength);
    const dec = new TextDecoder();
    const out = new Map();
    let p = 4;
    for (let i = 0, n = view.getUint32(0, true); i < n; i++) {
        const l = view.getUint32(p, true);
        const name = dec.decode(b.subarray(p + 4, p + 4 + l));
        p += 4 + l;
        const d = view.getUint32(p, true);
        out.set(name, b.subarray(p + 4, p + 4 + d));
        p += 4 + d;
    }
    return out;
}
/** Replaces a UTF-16 range; spans shift (typing at a span's end extends it), edits inside tate-chu-yoko dissolve it. */
export function replaceText(content, start, end, insert) {
    const c = structuredClone(content);
    if (start > end)
        [start, end] = [end, start];
    const added = insert.length, removed = end - start;
    // Text inserted at a boundary joins the span ending there, so typing continues the preceding style.
    const shift = (p) => (p < start ? p : p >= end ? p - removed + added : start + added);
    c.text = c.text.slice(0, start) + insert + c.text.slice(end);
    // Typing over a selection keeps the style of the replaced text.
    c.spans = (c.spans || []).map((s) => ({ ...s, start: s.start === start && removed > 0 ? s.start : shift(s.start), end: s.end === start ? s.end + added : shift(s.end) })).filter((s) => s.start < s.end);
    c.tcy = (c.tcy || [])
        .filter((r) => !(start < r.end && end > r.start) && !(start === end && start > r.start && start < r.end))
        .map((r) => ({ start: shift(r.start), end: shift(r.end) }));
    return c;
}
/** Applies a style patch to a UTF-16 range, splitting and merging spans. */
export function applyTextStyle(content, start, end, patch) {
    const c = structuredClone(content);
    if (start > end)
        [start, end] = [end, start];
    const spans = c.spans || [];
    const cuts = [...new Set([0, c.text.length, start, end, ...spans.flatMap((s) => [s.start, s.end])])].filter((x) => x >= 0 && x <= c.text.length).sort((a, b) => a - b);
    const out = [];
    for (let i = 0; i + 1 < cuts.length; i++) {
        const [a, b] = [cuts[i], cuts[i + 1]];
        let style = { ...(spans.find((s) => s.start <= a && b <= s.end)?.style || {}) };
        if (a >= start && b <= end)
            style = { ...style, ...patch };
        if (!Object.keys(style).length)
            continue;
        const last = out.at(-1);
        if (last && last.end === a && JSON.stringify(last.style) === JSON.stringify(style))
            last.end = b;
        else
            out.push({ start: a, end: b, style });
    }
    c.spans = out;
    return c;
}
export class AltoError extends Error {
    code;
    constructor(code, message) {
        super(message);
        this.code = code;
        this.name = "AltoError";
    }
}
class Events {
    listeners = new Map();
    on(name, fn) {
        let s = this.listeners.get(name);
        if (!s)
            this.listeners.set(name, (s = new Set()));
        s.add(fn);
        return () => s.delete(fn);
    }
    emit(name, value) {
        for (const f of this.listeners.get(name) || [])
            f(value);
    }
}
const readableAfterStorageFailure = new Set([
    "emergencyExport", "save", "view", "waitFrame", "readTile", "readTileLevel",
    "selectionOutline", "textInfo", "textHitTest", "textCaret",
    "textSelection", "textNavigate", "versionLog", "versionDiff",
    "vectorStats", "vectorPathSvg", "releaseSave", "snapshotHash", "nodeTiles",
]);
function nodeId() {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    let timestamp = BigInt(Date.now());
    for (let i = 5; i >= 0; i--) {
        bytes[i] = Number(timestamp & 255n);
        timestamp >>= 8n;
    }
    bytes[6] = (bytes[6] & 15) | 112;
    bytes[8] = (bytes[8] & 63) | 128;
    const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
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
    doc;
    ops = [];
    constructor(doc) {
        this.doc = doc;
    }
    addRaster(options = {}) {
        return this.insert("raster", options);
    }
    addGroup(options = {}) {
        return this.insert("group", options);
    }
    /** Registers an imported font in the document (required before text can use it). */
    addFont(font) {
        this.ops.push({ type: "set_asset", id: font.record.id, record: font.record });
        return font.record.id;
    }
    addText(content, options = {}) {
        const id = this.insert("text", { ...options, name: options.name || content.text.split("\n")[0].slice(0, 24) || "テキスト" });
        this.ops[this.ops.length - 1].node.kind = { kind: "text", content };
        return id;
    }
    setText(id, content) { this.ops.push({ type: "set_text", id, content }); }
    addVector(paths = [], options = {}) {
        const id = this.insert('vector', { ...options, name: options.name || 'ベクター' });
        this.ops[this.ops.length - 1].node.kind = { kind: 'vector', paths };
        return id;
    }
    setVector(id, paths) { this.ops.push({ type: 'set_vector', id, paths }); }
    setVectorMask(id, mask) { this.ops.push({ type: 'set_vector_mask', id, mask }); }
    setClipPath(id, path) { this.ops.push({ type: 'set_clip_path', id, path }); }
    addAdjustment(adjustment, options = {}) {
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
    setAdjustment(id, adjustment) {
        this.ops.push({ type: "set_adjustment", id, adjustment });
    }
    setMask(id, mask) {
        this.ops.push({ type: "set_mask", layer: id, mask });
    }
    insert(kind, options) {
        const id = nodeId();
        const parent = options.parent || this.doc.view.root.id;
        const existing = this.ops.find((o) => o.type === "insert_node" && o.node.id === parent);
        const count = existing ? 0 : this.doc.view.node(parent).children.length;
        const index = options.index ??
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
                kind: kind === "group"
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
    addRasterFromAsset(asset, options = {}) {
        const id = this.addRaster({ ...options, name: options.name || "画像" });
        this.ops[this.ops.length - 1].node.kind.tiles = asset.tiles;
        this.ops.push({
            type: "set_asset",
            id: asset.record.id,
            record: asset.record,
        });
        return id;
    }
    removeNode(id) {
        this.ops.push({ type: "remove_node", id });
    }
    moveNode(id, parent, index) {
        this.ops.push({ type: "move_node", id, parent, index });
    }
    setProps(id, patch) {
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
    setTransform(id, t) {
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
    mirror;
    history = { undo: 0, redo: 0 };
    /** Set by full views (open, save). Null after an edit: use `DocumentHandle.snapshotHash()`. */
    snapshotHash = null;
    stateHash = "";
    versions = { head: null, commits: 0, changed: false, changes: 0, pending_intents: 0 };
    dirty = false;
    warnings = [];
    apply(state) {
        if (state.patch) {
            this.mirror.revision = state.revision;
            if (Object.prototype.hasOwnProperty.call(state, "meta"))
                this.mirror.meta = state.meta;
            if (Object.prototype.hasOwnProperty.call(state, "selection"))
                this.mirror.selection = state.selection;
            if (Object.prototype.hasOwnProperty.call(state, "assets"))
                this.mirror.assets = state.assets;
            for (const id of state.removed ?? [])
                delete this.mirror.nodes[id];
            Object.assign(this.mirror.nodes, state.nodes ?? {});
        }
        else {
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
        if (old !== this.dirty)
            this.emit("dirty", this.dirty);
    }
    get selection() {
        return structuredClone(this.mirror.selection);
    }
    /** Asset records (images, fonts) registered in the document. */
    get assets() {
        return structuredClone(Object.values(this.mirror.assets || {}));
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
    node(id) {
        const n = this.mirror.nodes[id];
        if (!n)
            throw new AltoError("NOT_FOUND", "レイヤーが見つかりません");
        const children = (n.kind.children || []).map((id) => this.node(id));
        const copy = { ...structuredClone(n), children };
        const freeze = (v) => {
            if (v && typeof v === "object") {
                Object.values(v).forEach(freeze);
                Object.freeze(v);
            }
            return v;
        };
        return freeze(copy);
    }
    get nodes() {
        return Object.values(this.mirror.nodes).map((n) => this.node(n.id));
    }
}
export class ToolSession {
    doc;
    samples = [];
    ended = false;
    ready;
    constructor(doc, tool, options) {
        this.doc = doc;
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
    input(input) {
        if (this.ended)
            return;
        if ("clientX" in input) {
            const events = input.getCoalescedEvents?.() || [input];
            for (const e of events.length ? events : [input]) {
                const p = this.doc.engine.viewport.screenToDocument(e.clientX, e.clientY);
                this.samples.push(p[0], p[1], e.pointerType === "mouse" ? 1 : e.pressure, e.tiltX, e.tiltY, e.timeStamp);
            }
        }
        else
            this.samples.push(input.x, input.y, input.pressure ?? 1, input.tiltX ?? 0, input.tiltY ?? 0, input.time ?? performance.now());
    }
    async flush() {
        await this.ready;
        if (this.doc.engine.storageFailure)
            throw this.doc.engine.storageFailure;
        if (!this.samples.length)
            return;
        const a = new Float32Array(this.samples);
        this.samples = [];
        this.doc.engine.stream({ t: "input", samples: a }, [a.buffer]);
    }
    async end() {
        if (this.ended)
            return;
        this.ended = true;
        await this.flush();
        try {
            const state = await this.doc.engine.request("endBrush");
            this.doc.apply(state);
            return state.document?.revision ?? state.revision;
        }
        finally {
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
    engine;
    view = new DocumentView();
    activeTool = null;
    /** The folder this document was opened from or last saved to, and its manifest hash. */
    folder = null;
    constructor(engine, state) {
        this.engine = engine;
        this.apply(state);
    }
    apply(s) {
        this.view.apply(s);
        this.engine.emit("change", s);
        this.engine.emit("dirty", s.dirty);
    }
    textOf(id) {
        const n = this.view.node(id);
        if (n.kind.kind !== "text")
            throw new AltoError("INVALID_ARGUMENT", "テキストレイヤーではありません");
        return structuredClone(n.kind.content);
    }
    text = {
        set: (id, content) => this.transact("テキスト編集", (tx) => tx.setText(id, content)),
        /** Replaces a UTF-16 range of the text in one undoable edit. */
        replace: (id, start, end, insert, intent = "文字入力") => this.transact(intent, (tx) => tx.setText(id, replaceText(this.textOf(id), start, end, insert))),
        style: (id, start, end, patch) => this.transact("文字書式", (tx) => tx.setText(id, applyTextStyle(this.textOf(id), start, end, patch))),
        setTateChuYoko: (id, start, end, on = true) => this.transact("縦中横", (tx) => {
            const c = this.textOf(id);
            const rest = (c.tcy || []).filter((r) => r.end <= start || r.start >= end);
            c.tcy = on ? [...rest, { start, end }].sort((a, b) => a.start - b.start) : rest;
            tx.setText(id, c);
        }),
        info: (id) => this.engine.request("textInfo", { node: id }),
        /** UTF-16 offset nearest to a document point. */
        hitTest: (id, x, y) => this.engine.request("textHitTest", { node: id, x, y }),
        caret: (id, offset) => this.engine.request("textCaret", { node: id, offset }),
        /** Highlight quads in document coordinates. */
        selectionRects: (id, start, end) => this.engine.request("textSelection", { node: id, start, end }),
        navigate: (id, offset, motion) => this.engine.request("textNavigate", { node: id, offset, motion }),
    };
    versions = {
        /** Records the current document as a version. Fails when nothing changed unless allowEmpty. */
        commit: async (message, options = {}) => {
            if (this.activeTool)
                await this.activeTool.end();
            return this.engine.mutate(async () => {
                const r = await this.engine.request("commit", { message, author: options.author || "", allowEmpty: !!options.allowEmpty, time: Date.now() });
                this.apply(r.state);
                return r.commit;
            });
        },
        log: () => this.engine.request("versionLog"),
        /** Semantic, layer-level diff. Use "working" for the current document. */
        diff: (from, to = "working") => this.engine.request("versionDiff", { from, to }),
        status: () => this.view.versions,
        /** Restores a version as one undoable edit; history is kept. */
        restore: (hash) => this.command("restoreVersion", { hash }),
        /** Drops versions that cannot be reached from the head. Commit and open keep every version. */
        prune: async () => {
            const state = await this.engine.request("pruneVersions");
            this.apply(state);
            return state.removed ?? 0;
        },
    };
    /** Saves the folder form. Content-addressed files are written once; manifest.json last. */
    async saveFolder(provider, options = {}) {
        if (this.activeTool)
            await this.activeTool.end();
        this.engine.emit("saveState", "saving");
        let token;
        try {
            const current = await provider.readRef("manifest.json");
            const currentHash = current ? await sha256Hex(current) : "";
            const base = this.folder?.provider === provider ? this.folder.manifest : undefined;
            if (current && base === undefined && !options.overwrite)
                throw new AltoError("CONFLICT", "フォルダには既に別の文書があります");
            if (base !== undefined && base !== currentHash)
                throw new AltoError("CONFLICT", "フォルダの manifest.json が他で変更されています");
            const saved = await this.engine.request("folderEntries");
            token = saved.token;
            const entries = unframeEntries(new Uint8Array(saved.bytes));
            const manifest = entries.get("manifest.json");
            if (!manifest)
                throw new AltoError("INTERNAL", "manifest missing");
            const objects = [...entries].filter(([p]) => immutable(p)).map(([path, data]) => ({ path, data }));
            const report = await provider.putObjects(objects);
            for (const [p, d] of entries)
                if (!immutable(p) && p !== "manifest.json")
                    await provider.writeRef(p, d);
            await provider.writeRef("manifest.json", manifest, base ?? currentHash);
            const check = await provider.readRef("manifest.json");
            if (!check || check.length !== manifest.length || check.some((b, i) => b !== manifest[i]))
                throw new AltoError("STORAGE", "manifest の検証に失敗しました");
            this.folder = { provider, manifest: await sha256Hex(manifest) };
            this.apply(await this.engine.request("saved", { token: saved.token }));
            token = undefined;
            this.engine.emit("saveState", "saved");
            return report;
        }
        catch (e) {
            this.releaseSave(token);
            this.engine.emit("saveState", "error");
            throw e;
        }
    }
    vectors = {
        set: (id, paths) => this.transact('Edit vector paths', tx => tx.setVector(id, paths)),
        setMask: (id, mask) => this.transact('Vector mask', tx => tx.setVectorMask(id, mask)),
        setClipPath: (id, path) => this.transact('Clip path', tx => tx.setClipPath(id, path)),
    };
    tools = {
        begin: (tool, options) => {
            if (this.engine.storageFailure)
                throw this.engine.storageFailure;
            if (this.activeTool)
                throw new AltoError("CONFLICT", "ツールが使用中です");
            return (this.activeTool = new ToolSession(this, tool, options));
        },
    };
    async transact(intent, callback) {
        if (this.activeTool)
            await this.activeTool.end();
        return this.engine.mutate(async () => {
            const tx = new TransactionBuilder(this);
            const value = callback(tx);
            if (value && typeof value.then === "function")
                throw new AltoError("INVALID_ARGUMENT", "Transaction callback must be synchronous");
            const state = await this.engine.request("transact", {
                intent,
                ops: tx.ops,
                baseRevision: this.view.revision,
            });
            this.apply(state);
            return value;
        });
    }
    /**
     * Resolves once every edit made so far is in the recovery journal. Edits resolve
     * when applied; this rejects with STORAGE if the journal could not be written.
     */
    durable() {
        return this.engine.request("durable");
    }
    /** The hash of the snapshot a save would write. Computed on request, not per edit. */
    snapshotHash() {
        return this.engine.request("snapshotHash");
    }
    /** A raster layer's full tile map (key `tx,ty` to content hash). */
    nodeTiles(id) {
        return this.engine.request("nodeTiles", { node: id });
    }
    async undo() {
        if (this.activeTool)
            await this.activeTool.end();
        return this.engine.mutate(async () => this.apply(await this.engine.request("undo")));
    }
    async redo() {
        if (this.activeTool)
            await this.activeTool.end();
        return this.engine.mutate(async () => this.apply(await this.engine.request("redo")));
    }
    async command(type, payload) {
        if (this.activeTool)
            await this.activeTool.end();
        return this.engine.mutate(async () => {
            this.apply(await this.engine.request(type, payload));
        });
    }
    selection = {
        apply: (spec) => this.command("select", { spec }),
        outline: () => this.engine.request("selectionOutline"),
        clear: () => this.command("select", { spec: { kind: "clear" } }),
        invert: () => this.command("select", { spec: { kind: "invert" } }),
        all: () => this.command("select", { spec: { kind: "all" } }),
    };
    masks = {
        fromSelection: (layer) => this.command("setMask", { layer, source: "selection" }),
        addWhite: (layer) => this.command("setMask", { layer, source: "white" }),
        remove: (layer) => this.command("setMask", { layer, source: "remove" }),
    };
    effects = {
        set: (layer, effects, options = {}) => this.command("setEffects", {
            layer,
            effects,
            useSelection: options.useSelection ?? true,
        }),
    };
    async save(target) {
        if (this.activeTool)
            await this.activeTool.end();
        this.engine.emit("saveState", "saving");
        let token;
        try {
            const saved = await this.engine.request("save");
            token = saved.token;
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
                }
                catch (e) {
                    await writer.abort();
                    throw e;
                }
            }
            this.apply(await this.engine.request("saved", { token: saved.token }));
            token = undefined;
            this.engine.emit("saveState", "saved");
            return target;
        }
        catch (e) {
            this.releaseSave(token);
            this.engine.emit("saveState", "error");
            throw e;
        }
    }
    /** A cancelled or failed save no longer needs the Worker's copy of the package. */
    releaseSave(token) {
        if (token !== undefined)
            this.engine.request("releaseSave", { token }).catch(() => { });
    }
}
export class AltoEngine extends Events {
    canvas;
    worker;
    sequence = 1;
    pending = new Map();
    mutation = Promise.resolve();
    raf = 0;
    disposed = false;
    pixelRatio = Math.min(devicePixelRatio || 1, 2);
    phase = "booting";
    workerGeneration = 0;
    restartPromise = null;
    releaseLease;
    onPageHide = () => this.dispose();
    onWindowResize = () => this.resize();
    boot;
    options;
    document = null;
    recoveryAvailable = false;
    /** Set when the working journal can no longer confirm writes. Edits stay frozen until recovery. */
    storageFailure = null;
    /** A Worker startup or runtime failure. Independent of journal corruption. */
    engineFailure = null;
    get lifecycleState() { return this.phase; }
    get effectiveDpr() { return this.pixelRatio; }
    constructor(options) {
        super();
        this.options = options;
        this.canvas = options.canvas;
        window.addEventListener("pagehide", this.onPageHide);
        window.addEventListener("resize", this.onWindowResize);
    }
    static async create(options) {
        if (!options.canvas.transferControlToOffscreen || !navigator.gpu)
            throw new AltoError("GPU_UNSUPPORTED", "WebGPU対応ブラウザが必要です");
        const e = new AltoEngine(options);
        try {
            await e.acquireLease();
            await e.start();
            e.phase = "running";
            e.startLoop();
            return e;
        }
        catch (error) {
            e.dispose();
            throw error;
        }
    }
    async acquireLease() {
        if (!navigator.locks) {
            throw new AltoError("CONFLICT", "このブラウザは Web Locks が無く、同じ文書を別タブと同時に開けません");
        }
        let release;
        const hold = new Promise((resolve) => {
            release = resolve;
        });
        await new Promise((resolve, reject) => {
            void navigator.locks
                .request("alto-engine-working-journal", { ifAvailable: true }, async (lock) => {
                if (!lock) {
                    reject(new AltoError("CONFLICT", "同じサイトのAlto Engineが別のタブで動作しています。先にそのタブを閉じてください。"));
                    return;
                }
                this.releaseLease = release;
                resolve();
                await hold;
            })
                .catch(reject);
        });
    }
    async start() {
        this.boot = undefined;
        this.worker = new Worker(this.options.workerURL || new URL("./worker.js", import.meta.url), { type: "module" });
        const worker = this.worker;
        const generation = ++this.workerGeneration;
        this.worker.onmessage = ({ data }) => {
            if (generation !== this.workerGeneration || this.disposed)
                return;
            if (data.t === "reply") {
                const p = this.pending.get(data.id);
                if (!p)
                    return;
                this.pending.delete(data.id);
                if (p.timer)
                    clearTimeout(p.timer);
                data.ok
                    ? p.resolve(data.value)
                    : p.reject(new AltoError(data.error.code, data.error.message));
            }
            else {
                if (data.t === "patch") {
                    this.document?.apply(data.value);
                }
                else if (data.t === "storageFailure") {
                    if (!this.storageFailure) {
                        this.storageFailure = new AltoError("STORAGE", data.value.message);
                        if (this.document)
                            this.document.activeTool = null;
                        this.emit("storageFailure", this.storageFailure);
                    }
                }
                else if (data.t === "frame")
                    this.emit("frame", data.value);
                else if (data.t === "gpuLost") {
                    if (this.document)
                        this.document.activeTool = null;
                    this.emit("gpuLost", null);
                }
                else
                    this.emit(data.t, data.value);
            }
        };
        this.worker.onerror = (event) => {
            if (generation !== this.workerGeneration || this.disposed)
                return;
            const detail = event.message || event.error?.message || "";
            const e = new AltoError(this.phase === "running" ? "WORKER_CRASHED" : "WORKER_UNAVAILABLE", detail || (this.phase === "running" ? "Workerが停止しました" : "Workerを読み込めませんでした"));
            this.emit("workerDiagnostic", {
                phase: this.phase,
                workerURL: String(this.options.workerURL || new URL("./worker.js", import.meta.url)),
                message: event.message || null,
                file: event.filename || null,
                line: event.lineno || null,
            });
            this.rejectPending(e);
            if (this.phase === "running") {
                void this.restart().catch((failure) => this.emit("error", failure));
            }
        };
        const offscreen = this.canvas.transferControlToOffscreen();
        this.boot = await this.requestCore("init", {
            protocol: 4,
            sdk: "0.4.0",
            canvas: offscreen,
            budget: this.options.gpu?.memoryBudgetMB ?? 512,
        }, [offscreen], true, 30000);
        if (this.worker !== worker || this.disposed)
            throw new AltoError("WORKER_UNAVAILABLE", "Workerの起動が中断されました");
        if (this.boot.protocol !== 4)
            throw new AltoError("CONFLICT", "SDK/WASM protocol mismatch");
        this.recoveryAvailable = !!this.boot.recovery.available;
        if (this.recoveryAvailable)
            this.emit("recoveryAvailable", this.boot.recovery);
        this.resize();
    }
    startLoop() {
        const generation = this.workerGeneration;
        const loop = () => {
            if (this.disposed || this.phase !== "running" || generation !== this.workerGeneration)
                return;
            void this.document?.activeTool
                ?.flush()
                .catch((e) => this.emit("error", e));
            this.stream({ t: "tick", time: performance.now() });
            this.raf = requestAnimationFrame(loop);
        };
        this.raf = requestAnimationFrame(loop);
    }
    request(t, payload = {}, transfer = []) {
        return this.requestCore(t, payload, transfer, false);
    }
    requestCore(t, payload = {}, transfer = [], internal = false, timeoutMs = 0) {
        if (this.disposed)
            return Promise.reject(new AltoError("WORKER_UNAVAILABLE", "Engineは終了しました"));
        if (!internal && this.phase !== "running")
            return Promise.reject(this.engineFailure || new AltoError("WORKER_UNAVAILABLE", "Engineは復旧中です"));
        if (this.storageFailure && !readableAfterStorageFailure.has(t) &&
            !(t === "open" && this.storageFailure.code === "CORRUPT_DATA") && !internal)
            return Promise.reject(this.storageFailure);
        const id = this.sequence++;
        return new Promise((resolve, reject) => {
            const timer = timeoutMs ? setTimeout(() => {
                this.pending.delete(id);
                reject(new AltoError("WORKER_TIMEOUT", `${t} の応答がありません`));
            }, timeoutMs) : undefined;
            this.pending.set(id, { resolve, reject, timer });
            // Request fields come last so a payload can never replace the reply id.
            try {
                this.worker.postMessage({ ...payload, t, id }, transfer);
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
    stream(message, transfer = []) {
        if (this.disposed || this.phase === "failed" || !this.worker)
            return;
        this.worker.postMessage(message, transfer);
    }
    mutate(fn) {
        const p = this.mutation.then(() => {
            if (this.engineFailure)
                throw this.engineFailure;
            if (this.storageFailure)
                throw this.storageFailure;
            return fn();
        });
        this.mutation = p.catch(() => { });
        return p;
    }
    resize() {
        const r = this.canvas.getBoundingClientRect();
        const ratio = Math.min(devicePixelRatio || 1, 2);
        if (ratio !== this.pixelRatio) {
            const cssZoom = this.viewport.zoom / this.pixelRatio;
            this.pixelRatio = ratio;
            if (this.phase === "running")
                this.viewport.set({ zoom: cssZoom * ratio });
        }
        this.stream({
            t: "resize",
            width: Math.max(1, Math.round(r.width * ratio)),
            height: Math.max(1, Math.round(r.height * ratio)),
        });
    }
    async createDocument(meta) {
        const state = await this.request("createDocument", meta);
        this.document = new DocumentHandle(this, state);
        this.viewport.set({
            center: [meta.width / 2, meta.height / 2],
            zoom: 1,
            rotation: 0,
        });
        return this.document;
    }
    async openDocument(source) {
        let blob;
        if (source instanceof Blob)
            blob = source;
        else if (source.readArchive)
            blob = await source.readArchive();
        else
            blob = await source.getFile();
        const bytes = await blob.arrayBuffer();
        const state = await this.request("open", { bytes }, [bytes]);
        const reopeningAfterDamage = this.storageFailure?.code === "CORRUPT_DATA";
        if (reopeningAfterDamage)
            this.storageFailure = null;
        this.document = new DocumentHandle(this, state);
        this.viewport.set({
            center: [state.document.meta.width / 2, state.document.meta.height / 2],
            zoom: 1,
            rotation: 0,
        });
        if (reopeningAfterDamage)
            this.emit("recovered", null);
        return this.document;
    }
    /** Opens the folder form, reading only objects reachable from the document and its history. */
    async openFolder(provider) {
        const manifest = await provider.readRef("manifest.json");
        if (!manifest)
            throw new AltoError("NOT_FOUND", "manifest.json がありません");
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
    /** Exports the live runtime without touching the failed journal or marking it saved. */
    async exportEmergencyBytes() {
        return new Uint8Array(await this.request("emergencyExport"));
    }
    /** Recreates both workers and replays the confirmed journal after a storage failure. */
    async recoverAfterStorageFailure() {
        if (!this.storageFailure)
            throw new AltoError("CONFLICT", "保存障害は発生していません");
        const before = this.document && {
            revision: this.document.view.revision,
            stateHash: this.document.view.stateHash,
        };
        const failure = this.storageFailure;
        try {
            await this.restart();
        }
        catch (error) {
            this.storageFailure ||= failure;
            throw error;
        }
        if (!this.document)
            throw new AltoError("NOT_FOUND", "復旧できる記録がありません");
        return {
            document: this.document,
            beforeRevision: before?.revision ?? null,
            recoveredRevision: this.document.view.revision,
            matchesDisplay: before ? before.stateHash === this.document.view.stateHash : null,
        };
    }
    assets = {
        /** Imports an OpenType/TrueType font (.otf/.ttf/.ttc). Add it with tx.addFont(). */
        importFont: async (file, options = {}) => {
            const bytes = await file.arrayBuffer();
            return this.request("importFont", { bytes, faceIndex: options.faceIndex ?? 0 }, [bytes]);
        },
        /** Abandons an imported font or encoded image that was never added with SetAsset. */
        releaseImport: (hash) => this.request("releaseImport", { hash }),
        importImage: async (file) => {
            const bitmap = await createImageBitmap(file);
            const c = new OffscreenCanvas(bitmap.width, bitmap.height);
            const ctx = c.getContext("2d", { willReadFrequently: true });
            if (!ctx)
                throw new AltoError("INTERNAL", "画像デコード失敗");
            ctx.drawImage(bitmap, 0, 0);
            bitmap.close();
            const pixels = ctx.getImageData(0, 0, c.width, c.height).data;
            const encoded = await file.arrayBuffer();
            return this.request("import", {
                width: c.width,
                height: c.height,
                pixels: pixels.buffer,
                encoded,
                mime: file.type || "image/png",
            }, [pixels.buffer, encoded]);
        },
    };
    viewport = {
        version: 0,
        center: [512, 384],
        zoom: 1,
        rotation: 0,
        getCssZoom: () => this.viewport.zoom / this.pixelRatio,
        setCssZoom: (zoom) => this.viewport.set({ zoom: zoom * this.pixelRatio }),
        set: (v) => {
            if (v.center)
                this.viewport.center = v.center;
            if (v.zoom !== undefined)
                this.viewport.zoom = v.zoom;
            if (v.rotation !== undefined)
                this.viewport.rotation = v.rotation;
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
        screenToDocument: (x, y) => {
            const r = this.canvas.getBoundingClientRect();
            const ratio = this.pixelRatio;
            const dx = ((x - r.left - r.width / 2) * ratio) / this.viewport.zoom;
            const dy = ((y - r.top - r.height / 2) * ratio) / this.viewport.zoom;
            const c = Math.cos(this.viewport.rotation), s = Math.sin(this.viewport.rotation);
            return [
                this.viewport.center[0] + c * dx + s * dy,
                this.viewport.center[1] - s * dx + c * dy,
            ];
        },
    };
    storage = {
        /** Picks a folder for the `.altoproj` form (File System Access API). */
        pickFolder: async () => {
            if (!("showDirectoryPicker" in window))
                throw new AltoError("STORAGE", "このブラウザはフォルダへの保存に対応していません");
            return new FileSystemDirectoryProvider(await window.showDirectoryPicker({ mode: "readwrite" }));
        },
        /** A folder inside the origin private file system. */
        opfsFolder: async (name) => {
            const root = await navigator.storage.getDirectory();
            return new FileSystemDirectoryProvider(await root.getDirectoryHandle(name, { create: true }));
        },
        pickSaveFile: async (options) => {
            if ("showSaveFilePicker" in window)
                return window.showSaveFilePicker({
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
                writeArchive: async (stream) => {
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
    vectors = {
        parseSvg: (svg) => this.request('parseVectorPath', { svg }),
        toSvg: (data) => this.request('vectorPathSvg', { data }),
    };
    unstable = {
        settle: () => this.request("waitFrame"),
        readTile: async (x, y) => new Uint8Array(await this.request("readTile", { x, y })),
        readTileLevel: async (level, x, y) => new Uint8Array(await this.request("readTileLevel", { level, x, y })),
        vectorStats: () => this.request("vectorStats"),
        /** Small text through the glyph atlas (default) or always through the vector path. */
        setGlyphAtlas: (enabled) => this.request("setGlyphAtlas", { enabled }),
        destroyGpu: () => this.request("destroyGpu"),
        crash: () => this.restart(),
        /** Temporarily changes the module URL for Worker load-failure tests. */
        setWorkerURL: (url) => { this.options.workerURL = url; },
        pendingRequests: () => this.pending.size,
        /** Generation, record count and object count of the OPFS recovery store. */
        recoveryStore: () => this.request("recoveryStore"),
        corruptJournalTail: () => this.request("corruptJournalTail"),
        corruptCheckpoint: () => this.request("corruptCheckpoint"),
        corruptCheckpointCrc: () => this.request("corruptCheckpointCrc"),
        corruptBothCheckpoints: () => this.request("corruptBothCheckpoints"),
        legacyCheckpoint: () => this.request("legacyCheckpoint"),
        /** Fault injection for recovery tests only. */
        injectStorageFault: (position) => this.request("injectStorageFault", { position }),
        saveBytes: async () => {
            if (this.document?.activeTool)
                await this.document.activeTool.end();
            return new Uint8Array((await this.request("save")).bytes);
        },
    };
    /** Retries a failed Worker without discarding the confirmed journal. */
    retryWorker() {
        if (this.phase !== "failed")
            return Promise.reject(new AltoError("CONFLICT", "Workerは失敗状態ではありません"));
        const previous = this.restartPromise;
        return previous ? previous.catch(() => { }).then(() => this.restart()) : this.restart();
    }
    restart() {
        if (this.disposed)
            return Promise.reject(new AltoError("WORKER_UNAVAILABLE", "Engineは終了しました"));
        if (this.restartPromise)
            return this.restartPromise;
        this.phase = "recovering";
        this.engineFailure = null;
        if (this.document)
            this.document.activeTool = null;
        const task = Promise.resolve().then(() => this.performRestart());
        this.restartPromise = task;
        this.emit("recovering", null);
        void task.then(() => { if (this.restartPromise === task)
            this.restartPromise = null; }, () => { if (this.restartPromise === task)
            this.restartPromise = null; });
        return task;
    }
    async performRestart() {
        cancelAnimationFrame(this.raf);
        ++this.workerGeneration;
        this.worker?.terminate();
        this.rejectPending(new AltoError("WORKER_CRASHED", "Workerを再起動しました"));
        const replacement = this.canvas.cloneNode(false);
        this.canvas.replaceWith(replacement);
        this.canvas = replacement;
        this.emit("canvasReplaced", replacement);
        try {
            await this.start();
            if (this.disposed)
                throw new AltoError("WORKER_UNAVAILABLE", "Engineは終了しました");
            if (this.document || this.recoveryAvailable) {
                const state = await this.requestCore("recover", {}, [], true, 30000);
                if (this.disposed)
                    throw new AltoError("WORKER_UNAVAILABLE", "Engineは終了しました");
                this.document = new DocumentHandle(this, state);
                this.recoveryAvailable = false;
                this.viewport.set({
                    center: [state.document.meta.width / 2, state.document.meta.height / 2],
                    zoom: 1,
                    rotation: 0,
                });
            }
            this.storageFailure = null;
            this.engineFailure = null;
            this.phase = "running";
            this.startLoop();
            this.emit("recovered", null);
        }
        catch (cause) {
            if (this.disposed)
                throw cause;
            const error = cause instanceof AltoError
                ? cause
                : new AltoError("WORKER_UNAVAILABLE", cause instanceof Error ? cause.message : String(cause));
            this.document = null;
            if (error.code === "CORRUPT_DATA") {
                this.storageFailure = error;
                this.phase = "running";
                this.startLoop();
                this.emit("storageFailure", error);
            }
            else {
                this.engineFailure = error;
                this.phase = "failed";
                this.worker?.terminate();
                this.emit("engineFailure", error);
            }
            throw error;
        }
    }
    dispose() {
        this.disposed = true;
        this.phase = "disposed";
        ++this.workerGeneration;
        cancelAnimationFrame(this.raf);
        this.worker?.terminate();
        this.releaseLease?.();
        this.releaseLease = undefined;
        window.removeEventListener("pagehide", this.onPageHide);
        window.removeEventListener("resize", this.onWindowResize);
        this.rejectPending(new AltoError("WORKER_UNAVAILABLE", "Engineは終了しました"));
    }
}
