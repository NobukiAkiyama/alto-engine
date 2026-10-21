export class AltoError extends Error {
    code;
    constructor(code, message) {
        super(message);
        this.code = code;
        this.name = 'AltoError';
    }
}
class Events {
    listeners = new Map();
    on(name, fn) { let s = this.listeners.get(name); if (!s)
        this.listeners.set(name, s = new Set()); s.add(fn); return () => s.delete(fn); }
    emit(name, value) { for (const f of this.listeners.get(name) || [])
        f(value); }
}
const props = () => ({ visible: true, locked: false, opacity: 1, blend: 'normal', transform: [1, 0, 0, 1, 0, 0], mask: null, clip_to_below: false, effects: [] });
export class TransactionBuilder {
    doc;
    ops = [];
    constructor(doc) {
        this.doc = doc;
    }
    addRaster(options = {}) { return this.insert('raster', options); }
    addGroup(options = {}) { return this.insert('group', options); }
    insert(kind, options) { const id = crypto.randomUUID(); const parent = options.parent || this.doc.view.root.id; const index = options.index ?? this.doc.view.node(parent).children.length; this.ops.push({ type: 'insert_node', node: { id, parent: null, name: options.name || (kind === 'group' ? 'グループ' : 'レイヤー'), props: props(), kind: kind === 'group' ? { kind, children: [], isolation: options.isolated ? 'isolated' : 'pass_through' } : { kind, tiles: { format: 'rgba8', tiles: {} } } }, parent, index: index + this.ops.filter(o => o.type === 'insert_node' && o.parent === parent && o.index === index).length }); return id; }
    addRasterFromAsset(asset, options = {}) { const id = this.addRaster({ ...options, name: options.name || '画像' }); this.ops[this.ops.length - 1].node.kind.tiles = asset.tiles; this.ops.push({ type: 'set_asset', id: asset.record.id, record: asset.record }); return id; }
    removeNode(id) { this.ops.push({ type: 'remove_node', id }); }
    moveNode(id, parent, index) { this.ops.push({ type: 'move_node', id, parent, index }); }
    setProps(id, patch) { this.ops.push({ type: 'set_props', id, patch: { name: null, visible: null, locked: null, opacity: null, blend: null, transform: null, clip_to_below: null, effects: null, ...patch } }); }
    setTransform(id, t) { const m = Array.isArray(t) ? t : [Math.cos(t.rotation) * t.scaleX, Math.sin(t.rotation) * t.scaleX, -Math.sin(t.rotation) * t.scaleY, Math.cos(t.rotation) * t.scaleY, t.x, t.y]; this.setProps(id, { transform: m }); }
}
class DocumentView extends Events {
    mirror;
    history = { undo: 0, redo: 0 };
    snapshotHash = '';
    dirty = false;
    warnings = [];
    apply(state) { this.mirror = state.document; this.history = state.history; this.snapshotHash = state.snapshotHash; const old = this.dirty; this.dirty = state.dirty; this.warnings = state.warnings || []; this.emit('change', state); if (old !== this.dirty)
        this.emit('dirty', this.dirty); }
    get root() { return this.node(this.mirror.root); }
    get meta() { return Object.freeze({ ...this.mirror.meta }); }
    get revision() { return this.mirror.revision; }
    node(id) { const n = this.mirror.nodes[id]; if (!n)
        throw new AltoError('NOT_FOUND', 'レイヤーが見つかりません'); const children = (n.kind.children || []).map(id => this.node(id)); return Object.freeze({ ...n, props: Object.freeze({ ...n.props, transform: Object.freeze([...n.props.transform]) }), children }); }
    get nodes() { return Object.values(this.mirror.nodes).map(n => this.node(n.id)); }
}
export class ToolSession {
    doc;
    samples = [];
    ended = false;
    ready;
    constructor(doc, tool, options) {
        this.doc = doc;
        this.ready = doc.engine.request('beginBrush', { layer: options.layer, config: { size: options.tip?.size ?? 24, hardness: options.tip?.hardness ?? 0.8, spacing: options.tip?.spacing ?? 0.15, opacity: options.opacity ?? 1, flow: options.flow ?? 1, pressure_size: options.pressureSize ?? true, pressure_opacity: options.pressureOpacity ?? false }, color: options.color ?? [0, 0, 0, 1], erase: tool === 'eraser' });
        this.ready.catch(e => doc.engine.emit('error', e));
    }
    input(input) { if (this.ended)
        return; if ('clientX' in input) {
        const events = input.getCoalescedEvents?.() || [input];
        for (const e of events.length ? events : [input]) {
            const p = this.doc.engine.viewport.screenToDocument(e.clientX, e.clientY);
            this.samples.push(p[0], p[1], e.pointerType === 'mouse' ? 1 : e.pressure, e.tiltX, e.tiltY, e.timeStamp);
        }
    }
    else
        this.samples.push(input.x, input.y, input.pressure ?? 1, input.tiltX ?? 0, input.tiltY ?? 0, input.time ?? performance.now()); }
    async flush() { await this.ready; if (!this.samples.length)
        return; const a = new Float32Array(this.samples); this.samples = []; this.doc.engine.stream({ t: 'input', samples: a }, [a.buffer]); }
    async end() { if (this.ended)
        return; this.ended = true; await this.flush(); try {
        const state = await this.doc.engine.request('endBrush');
        this.doc.apply(state);
        return state.document.revision;
    }
    finally {
        this.doc.activeTool = null;
    } }
    async cancel() { this.ended = true; this.samples = []; await this.ready; await this.doc.engine.request('cancelBrush'); this.doc.activeTool = null; }
}
export class DocumentHandle {
    engine;
    view = new DocumentView();
    activeTool = null;
    constructor(engine, state) {
        this.engine = engine;
        this.apply(state);
    }
    apply(s) { this.view.apply(s); this.engine.emit('change', s); this.engine.emit('dirty', s.dirty); }
    tools = { begin: (tool, options) => { if (this.activeTool)
            throw new AltoError('CONFLICT', 'ツールが使用中です'); return this.activeTool = new ToolSession(this, tool, options); } };
    async transact(intent, callback) { if (this.activeTool)
        await this.activeTool.end(); return this.engine.mutate(async () => { const tx = new TransactionBuilder(this); const value = callback(tx); const state = await this.engine.request('transact', { intent, ops: tx.ops, baseRevision: this.view.revision }); this.apply(state); return value; }); }
    async undo() { if (this.activeTool)
        await this.activeTool.end(); return this.engine.mutate(async () => this.apply(await this.engine.request('undo'))); }
    async redo() { if (this.activeTool)
        await this.activeTool.end(); return this.engine.mutate(async () => this.apply(await this.engine.request('redo'))); }
    async save(target) { if (this.activeTool)
        await this.activeTool.end(); this.engine.emit('saveState', 'saving'); try {
        const saved = await this.engine.request('save');
        const data = new Uint8Array(saved.bytes);
        if (!target)
            target = await this.engine.storage.pickSaveFile({ suggestedName: 'work.alto' });
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
        this.apply(await this.engine.request('saved', { token: saved.token }));
        this.engine.emit('saveState', 'saved');
        return target;
    }
    catch (e) {
        this.engine.emit('saveState', 'error');
        throw e;
    } }
}
export class AltoEngine extends Events {
    canvas;
    worker;
    sequence = 1;
    pending = new Map();
    mutation = Promise.resolve();
    raf = 0;
    disposed = false;
    boot;
    options;
    document = null;
    recoveryAvailable = false;
    constructor(options) { super(); this.options = options; this.canvas = options.canvas; }
    static async create(options) { if (!options.canvas.transferControlToOffscreen || !navigator.gpu)
        throw new AltoError('GPU_UNSUPPORTED', 'WebGPU対応ブラウザが必要です'); const e = new AltoEngine(options); await e.start(); return e; }
    async start() { this.worker = new Worker(this.options.workerURL || new URL('./worker.js', import.meta.url), { type: 'module' }); this.worker.onmessage = ({ data }) => { if (data.t === 'reply') {
        const p = this.pending.get(data.id);
        if (!p)
            return;
        this.pending.delete(data.id);
        data.ok ? p.resolve(data.value) : p.reject(new AltoError(data.error.code, data.error.message));
    }
    else {
        if (data.t === 'frame')
            this.emit('frame', data.value);
        else if (data.t === 'gpuLost') {
            if (this.document)
                this.document.activeTool = null;
            this.emit('gpuLost', null);
        }
        else
            this.emit(data.t, data.value);
    } }; this.worker.onerror = (event) => { this.emit('error', new AltoError('INTERNAL', event.message)); void this.restart(); }; const offscreen = this.canvas.transferControlToOffscreen(); this.boot = await this.request('init', { canvas: offscreen, budget: this.options.gpu?.memoryBudgetMB ?? 512 }, [offscreen]); this.recoveryAvailable = !!this.boot.recovery.available; if (this.recoveryAvailable)
        this.emit('recoveryAvailable', this.boot.recovery); this.resize(); const loop = () => { if (this.disposed)
        return; void this.document?.activeTool?.flush().catch(e => this.emit('error', e)); this.stream({ t: 'tick', time: performance.now() }); this.raf = requestAnimationFrame(loop); }; this.raf = requestAnimationFrame(loop); }
    request(t, payload = {}, transfer = []) { const id = this.sequence++; return new Promise((resolve, reject) => { this.pending.set(id, { resolve, reject }); this.worker.postMessage({ t, id, ...payload }, transfer); }); }
    stream(message, transfer = []) { this.worker.postMessage(message, transfer); }
    mutate(fn) { const p = this.mutation.then(fn); this.mutation = p.catch(() => { }); return p; }
    resize() { const r = this.canvas.getBoundingClientRect(); const ratio = Math.min(devicePixelRatio || 1, 2); this.stream({ t: 'resize', width: Math.max(1, Math.round(r.width * ratio)), height: Math.max(1, Math.round(r.height * ratio)) }); }
    async createDocument(meta) { const state = await this.request('createDocument', meta); this.document = new DocumentHandle(this, state); this.viewport.set({ center: [meta.width / 2, meta.height / 2], zoom: 1, rotation: 0 }); return this.document; }
    async openDocument(source) { let blob; if (source instanceof Blob)
        blob = source;
    else if (source.readArchive)
        blob = await source.readArchive();
    else
        blob = await source.getFile(); const bytes = await blob.arrayBuffer(); const state = await this.request('open', { bytes }, [bytes]); this.document = new DocumentHandle(this, state); this.viewport.set({ center: [state.document.meta.width / 2, state.document.meta.height / 2], zoom: 1, rotation: 0 }); return this.document; }
    async recover() { const state = await this.request('recover'); this.document = new DocumentHandle(this, state); this.recoveryAvailable = false; this.viewport.set({ center: [state.document.meta.width / 2, state.document.meta.height / 2], zoom: 1, rotation: 0 }); return this.document; }
    assets = { importImage: async (file) => { const bitmap = await createImageBitmap(file); const c = new OffscreenCanvas(bitmap.width, bitmap.height); const ctx = c.getContext('2d', { willReadFrequently: true }); if (!ctx)
            throw new AltoError('INTERNAL', '画像デコード失敗'); ctx.drawImage(bitmap, 0, 0); bitmap.close(); const pixels = ctx.getImageData(0, 0, c.width, c.height).data; const encoded = await file.arrayBuffer(); return this.request('import', { width: c.width, height: c.height, pixels: pixels.buffer, encoded, mime: file.type || 'image/png' }, [pixels.buffer, encoded]); } };
    viewport = { center: [512, 384], zoom: 1, rotation: 0, set: (v) => { if (v.center)
            this.viewport.center = v.center; if (v.zoom !== undefined)
            this.viewport.zoom = v.zoom; if (v.rotation !== undefined)
            this.viewport.rotation = v.rotation; this.stream({ t: 'viewport', center: this.viewport.center, zoom: this.viewport.zoom, rotation: this.viewport.rotation }); }, screenToDocument: (x, y) => { const r = this.canvas.getBoundingClientRect(); const ratio = Math.min(devicePixelRatio || 1, 2); const dx = (x - r.left - r.width / 2) * ratio / this.viewport.zoom; const dy = (y - r.top - r.height / 2) * ratio / this.viewport.zoom; const c = Math.cos(this.viewport.rotation), s = Math.sin(this.viewport.rotation); return [this.viewport.center[0] + c * dx + s * dy, this.viewport.center[1] - s * dx + c * dy]; } };
    storage = { pickSaveFile: async (options) => { if ('showSaveFilePicker' in window)
            return window.showSaveFilePicker({ suggestedName: options.suggestedName, types: [{ description: 'Alto document', accept: { 'application/zip': ['.alto'] } }] }); return { kind: 'download', writeArchive: async (stream) => { const blob = await new Response(stream).blob(); const a = document.createElement('a'); a.href = URL.createObjectURL(blob); a.download = options.suggestedName; a.click(); setTimeout(() => URL.revokeObjectURL(a.href), 1000); } }; } };
    unstable = { readTile: async (x, y) => new Uint8Array(await this.request('readTile', { x, y })), destroyGpu: () => this.request('destroyGpu'), crash: () => this.restart(), saveBytes: async () => { if (this.document?.activeTool)
            await this.document.activeTool.end(); return new Uint8Array((await this.request('save')).bytes); } };
    async restart() { if (this.disposed)
        return; cancelAnimationFrame(this.raf); this.worker.terminate(); for (const p of this.pending.values())
        p.reject(new AltoError('INTERNAL', 'Worker restarted')); this.pending.clear(); const replacement = this.canvas.cloneNode(false); this.canvas.replaceWith(replacement); this.canvas = replacement; await this.start(); if (this.recoveryAvailable)
        await this.recover(); this.emit('canvasReplaced', replacement); this.emit('recovered', null); }
    dispose() { this.disposed = true; cancelAnimationFrame(this.raf); this.worker.terminate(); for (const p of this.pending.values())
        p.reject(new AltoError('INTERNAL', 'Engine disposed')); this.pending.clear(); }
}
