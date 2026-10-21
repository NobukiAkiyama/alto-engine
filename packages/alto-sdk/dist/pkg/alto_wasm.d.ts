/* tslint:disable */
/* eslint-disable */

export class Runtime {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    beginBrush(layer: string, config: string, color: Float32Array, erase: boolean, mask: boolean): void;
    brushInput(samples: Float32Array): void;
    cancelBrush(): void;
    /**
     * The canonical .alto bytes for a folder, used as the recovery checkpoint.
     */
    checkpointBytes(): Uint8Array;
    commit(message: string, created_at: number, author: string, allow_empty: boolean): string;
    cpuBrushReference(config: string, color: Float32Array, samples: Float32Array): Uint8Array;
    createDocument(w: number, h: number): string;
    static create(canvas: OffscreenCanvas, budget: number): Promise<Runtime>;
    destroyGpu(): void;
    endBrush(): Promise<string>;
    folderEntries(): Uint8Array;
    folderPlan(framed: Uint8Array): string;
    gpuIdle(): Promise<void>;
    importFont(bytes: Uint8Array, face_index: number): string;
    importRgba(w: number, h: number, pixels: Uint8Array, encoded: Uint8Array, mime: string): string;
    markSaved(hash: string): string;
    openFolder(framed: Uint8Array): string;
    openPackage(bytes: Uint8Array): string;
    /**
     * Only called by the journal worker after verifying the checkpoint digest.
     */
    openRecoveryPackage(bytes: Uint8Array, opaque: string): string;
    parseVectorPath(svg: string): string;
    protocolVersion(): number;
    pruneVersions(): string;
    readTileLevel(level: number, tx: number, ty: number): Promise<Uint8Array>;
    readTile(tx: number, ty: number): Promise<Uint8Array>;
    recoverGpu(): Promise<string>;
    recoveryOpaque(): string;
    redo(): string;
    releaseImport(hash: string): boolean;
    replayRecord(bytes: Uint8Array): string;
    resize(w: number, h: number): void;
    restoreVersion(hash: string): string;
    savePackage(): Uint8Array;
    select(spec: string): string;
    selectionOutline(): string;
    setEffects(layer: string, effects: string, use_selection: boolean): string;
    /**
     * Small text through the glyph atlas (default) or always through the vector path.
     */
    setGlyphAtlas(enabled: boolean): void;
    setMask(layer: string, source: string): string;
    setProfiling(enabled: boolean): void;
    takeJournal(): Uint8Array;
    textCaret(id: string, offset: number): string;
    textHitTest(id: string, x: number, y: number): number;
    textInfo(id: string): string;
    textNavigate(id: string, offset: number, motion: string): number;
    textSelection(id: string, start: number, end: number): string;
    tick(budget_ms: number): string;
    transact(request: string): string;
    undo(): string;
    /**
     * Exercises an uncached frame under a WebGPU validation error scope.
     */
    validateFrame(): Promise<void>;
    vectorPathSvg(data: string): string;
    vectorStats(): string;
    versionDiff(from: string, to: string): string;
    versionLog(): string;
    view(): string;
    viewport(x: number, y: number, zoom: number, rotation: number): void;
}

export class Spike {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    canRedo(): boolean;
    canUndo(): boolean;
    static create(canvas: OffscreenCanvas): Promise<Spike>;
    redo(): boolean;
    render(pan_x: number, pan_y: number, zoom: number): void;
    resize(width: number, height: number): void;
    rewriteAndReadTile(index: number): Promise<number>;
    snapshotJson(): string;
    undo(): boolean;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_runtime_free: (a: number, b: number) => void;
    readonly __wbg_spike_free: (a: number, b: number) => void;
    readonly runtime_beginBrush: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => [number, number];
    readonly runtime_brushInput: (a: number, b: number, c: number) => [number, number];
    readonly runtime_cancelBrush: (a: number) => void;
    readonly runtime_checkpointBytes: (a: number) => [number, number, number, number];
    readonly runtime_commit: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly runtime_cpuBrushReference: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly runtime_create: (a: any, b: number) => any;
    readonly runtime_createDocument: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_destroyGpu: (a: number) => void;
    readonly runtime_endBrush: (a: number) => any;
    readonly runtime_folderEntries: (a: number) => [number, number, number, number];
    readonly runtime_folderPlan: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_gpuIdle: (a: number) => any;
    readonly runtime_importFont: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly runtime_importRgba: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => [number, number, number, number];
    readonly runtime_markSaved: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_openFolder: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_openPackage: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_openRecoveryPackage: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly runtime_parseVectorPath: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_protocolVersion: (a: number) => number;
    readonly runtime_pruneVersions: (a: number) => [number, number, number, number];
    readonly runtime_readTile: (a: number, b: number, c: number) => any;
    readonly runtime_readTileLevel: (a: number, b: number, c: number, d: number) => any;
    readonly runtime_recoverGpu: (a: number) => any;
    readonly runtime_recoveryOpaque: (a: number) => [number, number, number, number];
    readonly runtime_redo: (a: number) => [number, number, number, number];
    readonly runtime_releaseImport: (a: number, b: number, c: number) => [number, number, number];
    readonly runtime_replayRecord: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_resize: (a: number, b: number, c: number) => void;
    readonly runtime_restoreVersion: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_savePackage: (a: number) => [number, number, number, number];
    readonly runtime_select: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_selectionOutline: (a: number) => [number, number, number, number];
    readonly runtime_setEffects: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly runtime_setGlyphAtlas: (a: number, b: number) => void;
    readonly runtime_setMask: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly runtime_setProfiling: (a: number, b: number) => void;
    readonly runtime_takeJournal: (a: number) => [number, number];
    readonly runtime_textCaret: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly runtime_textHitTest: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
    readonly runtime_textInfo: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_textNavigate: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number];
    readonly runtime_textSelection: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly runtime_tick: (a: number, b: number) => [number, number, number, number];
    readonly runtime_transact: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_undo: (a: number) => [number, number, number, number];
    readonly runtime_validateFrame: (a: number) => any;
    readonly runtime_vectorPathSvg: (a: number, b: number, c: number) => [number, number, number, number];
    readonly runtime_vectorStats: (a: number) => [number, number];
    readonly runtime_versionDiff: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly runtime_versionLog: (a: number) => [number, number, number, number];
    readonly runtime_view: (a: number) => [number, number, number, number];
    readonly runtime_viewport: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly spike_canRedo: (a: number) => number;
    readonly spike_canUndo: (a: number) => number;
    readonly spike_create: (a: any) => any;
    readonly spike_redo: (a: number) => [number, number, number];
    readonly spike_render: (a: number, b: number, c: number, d: number) => [number, number];
    readonly spike_resize: (a: number, b: number, c: number) => void;
    readonly spike_rewriteAndReadTile: (a: number, b: number) => any;
    readonly spike_snapshotJson: (a: number) => [number, number, number, number];
    readonly spike_undo: (a: number) => [number, number, number];
    readonly wasm_bindgen_4c6a9881086b567___convert__closures_____invoke___js_sys_fa6008ce7617d4a5___Function_fn_wasm_bindgen_4c6a9881086b567___JsValue_____wasm_bindgen_4c6a9881086b567___sys__Undefined___js_sys_fa6008ce7617d4a5___Function_fn_wasm_bindgen_4c6a9881086b567___JsValue_____wasm_bindgen_4c6a9881086b567___sys__Undefined_______true_: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen_4c6a9881086b567___convert__closures_____invoke___wasm_bindgen_4c6a9881086b567___JsValue__core_37ee0ce0c4cd57d5___result__Result_____wasm_bindgen_4c6a9881086b567___JsError___true_: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_4c6a9881086b567___convert__closures_____invoke___wasm_bindgen_4c6a9881086b567___sys__JsNullable_wgpu_f2aa5dc9c5719dad___backend__webgpu__webgpu_sys__gen_GpuError__GpuError___core_37ee0ce0c4cd57d5___result__Result_____wasm_bindgen_4c6a9881086b567___JsError___true_: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_4c6a9881086b567___convert__closures_____invoke___wasm_bindgen_4c6a9881086b567___sys__JsNullable_wgpu_f2aa5dc9c5719dad___backend__webgpu__webgpu_sys__gen_GpuError__GpuError___core_37ee0ce0c4cd57d5___result__Result_____wasm_bindgen_4c6a9881086b567___JsError___true__67: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_4c6a9881086b567___convert__closures_____invoke___wasm_bindgen_4c6a9881086b567___sys__JsNullable_wgpu_f2aa5dc9c5719dad___backend__webgpu__webgpu_sys__gen_GpuError__GpuError___core_37ee0ce0c4cd57d5___result__Result_____wasm_bindgen_4c6a9881086b567___JsError___true__68: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_4c6a9881086b567___convert__closures_____invoke___wgpu_f2aa5dc9c5719dad___backend__webgpu__webgpu_sys__gen_GpuDeviceLostInfo__GpuDeviceLostInfo______true_: (a: number, b: number, c: any) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_destroy_closure: (a: number, b: number) => void;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
