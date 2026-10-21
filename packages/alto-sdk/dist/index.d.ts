export type NodeId = string;
export type BlendMode = "normal" | "multiply" | "screen" | "overlay" | "darken" | "lighten" | "color_dodge" | "color_burn" | "hard_light" | "soft_light" | "difference" | "exclusion" | "hue" | "saturation" | "color" | "luminosity" | "add";
export type Color = [number, number, number, number];
export type PathCommand = {
    op: 'move' | 'line';
    x: number;
    y: number;
} | {
    op: 'quad';
    cx: number;
    cy: number;
    x: number;
    y: number;
} | {
    op: 'cubic';
    c1x: number;
    c1y: number;
    c2x: number;
    c2y: number;
    x: number;
    y: number;
} | {
    op: 'close';
};
export interface PathData {
    commands: PathCommand[];
}
export type FillRule = 'non_zero' | 'even_odd';
export interface VectorStroke {
    color: Color;
    width: number;
    cap?: 'butt' | 'round' | 'square';
    join?: 'miter' | 'round' | 'bevel';
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
    transform?: number[];
}
export interface ClipPath {
    path: PathData;
    fill_rule?: FillRule;
    enabled?: boolean;
    transform?: number[];
}
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
    frame?: {
        kind: "point";
    } | {
        kind: "area";
        width: number;
        height: number;
    };
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
export interface FontAsset {
    record: any;
    info: FontInfo;
    warnings: string[];
}
export interface TextInfo {
    lines: {
        start: number;
        end: number;
        block: [number, number];
        baseline: number;
    }[];
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
export type TextMotion = "forward" | "backward" | "line_start" | "line_end" | "next_line" | "previous_line" | "word_forward" | "word_backward" | "document_start" | "document_end";
export interface CommitView {
    hash: string;
    parents: string[];
    snapshot: string;
    message: string;
    created_at: number;
    author: string | null;
    intents: {
        intent: string;
        count: number;
        targets: string[];
        target_names: string[];
    }[];
}
export type DiffItem = {
    type: "added";
    id: string;
    name: string;
    kind: string;
    parent: string;
} | {
    type: "removed";
    id: string;
    name: string;
    kind: string;
} | {
    type: "moved";
    id: string;
    name: string;
    from: [string, number];
    to: [string, number];
} | {
    type: "props_changed";
    id: string;
    name: string;
    fields: string[];
} | {
    type: "pixels_changed";
    id: string;
    name: string;
    target: "content" | "mask";
    tiles: number;
    bounds: Rect;
    document_bounds: Rect;
} | {
    type: "content_changed";
    id: string;
    name: string;
    kind: string;
    detail: string;
} | {
    type: "asset_changed";
    id: string;
    change: "added" | "removed" | "changed";
    kind: string;
} | {
    type: "meta_changed";
    fields: string[];
} | {
    type: "selection_changed";
    tiles: number;
};
export interface Rect {
    x0: number;
    y0: number;
    x1: number;
    y1: number;
}
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
        mask: {
            format: string;
            tiles: Record<string, string>;
        } | null;
        mask_enabled: boolean;
        mask_linked: boolean;
        mask_transform: number[];
        vector_mask: VectorMask | null;
        clip_path: ClipPath | null;
        clip_to_below: boolean;
        effects: unknown[];
    };
    kind: {
        kind: string;
        children?: NodeId[];
        tiles?: {
            format: string;
            tiles: Record<string, string>;
        };
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
    selection: {
        format: string;
        tiles: Record<string, string>;
    } | null;
}
export interface AssetRef {
    record: any;
    tiles: {
        format: string;
        tiles: Record<string, string>;
    };
    width: number;
    height: number;
}
export interface BrushOptions {
    layer: NodeId;
    target?: "content" | "mask";
    tip?: {
        size?: number;
        hardness?: number;
        spacing?: number;
    };
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
    putObjects(entries: {
        path: string;
        data: Uint8Array;
    }[]): Promise<{
        written: number;
        skipped: number;
    }>;
    list(prefix: string): Promise<string[]>;
    readRef(name: string): Promise<Uint8Array | null>;
    writeRef(name: string, data: Uint8Array, expectHash?: string): Promise<void>;
}
export declare function sha256Hex(data: Uint8Array): Promise<string>;
export declare class MemoryObjectProvider implements ObjectStorageProvider {
    readonly kind = "memory";
    readonly files: Map<string, Uint8Array<ArrayBufferLike>>;
    getObject(path: string): Promise<Uint8Array<ArrayBuffer> | null>;
    putObjects(entries: {
        path: string;
        data: Uint8Array;
    }[]): Promise<{
        written: number;
        skipped: number;
    }>;
    list(prefix: string): Promise<string[]>;
    readRef(name: string): Promise<Uint8Array<ArrayBuffer> | null>;
    writeRef(name: string, data: Uint8Array, expectHash?: string): Promise<void>;
}
/** A directory from the File System Access API or OPFS. Writes replace atomically on close. */
export declare class FileSystemDirectoryProvider implements ObjectStorageProvider {
    readonly handle: FileSystemDirectoryHandle;
    readonly kind = "fs-access-dir";
    constructor(handle: FileSystemDirectoryHandle);
    private dir;
    private file;
    getObject(path: string): Promise<Uint8Array<ArrayBuffer> | null>;
    private write;
    putObjects(entries: {
        path: string;
        data: Uint8Array;
    }[]): Promise<{
        written: number;
        skipped: number;
    }>;
    private exists;
    list(prefix: string): Promise<string[]>;
    readRef(name: string): Promise<Uint8Array<ArrayBuffer> | null>;
    writeRef(name: string, data: Uint8Array, expectHash?: string): Promise<void>;
}
/** Worker <-> main framing for package entries. */
export declare function frameEntries(entries: Map<string, Uint8Array>): Uint8Array;
export declare function unframeEntries(b: Uint8Array): Map<string, Uint8Array>;
/** Replaces a UTF-16 range; spans shift (typing at a span's end extends it), edits inside tate-chu-yoko dissolve it. */
export declare function replaceText(content: TextContent, start: number, end: number, insert: string): TextContent;
/** Applies a style patch to a UTF-16 range, splitting and merging spans. */
export declare function applyTextStyle(content: TextContent, start: number, end: number, patch: StylePatch): TextContent;
export declare class AltoError extends Error {
    code: string;
    constructor(code: string, message: string);
}
declare class Events {
    private listeners;
    on(name: string, fn: (v: any) => void): () => boolean;
    emit(name: string, value: any): void;
}
export declare class TransactionBuilder {
    private doc;
    readonly ops: any[];
    constructor(doc: DocumentHandle);
    addRaster(options?: {
        parent?: string;
        index?: number;
        name?: string;
    }): string;
    addGroup(options?: {
        parent?: string;
        index?: number;
        name?: string;
        isolated?: boolean;
    }): string;
    /** Registers an imported font in the document (required before text can use it). */
    addFont(font: FontAsset | {
        record: any;
    }): string;
    addText(content: TextContent, options?: {
        parent?: string;
        index?: number;
        name?: string;
    }): string;
    setText(id: string, content: TextContent): void;
    addVector(paths?: StyledPath[], options?: {
        parent?: string;
        index?: number;
        name?: string;
    }): string;
    setVector(id: string, paths: StyledPath[]): void;
    setVectorMask(id: string, mask: VectorMask | null): void;
    setClipPath(id: string, path: ClipPath | null): void;
    addAdjustment(adjustment: {
        type: "color_adjust";
        brightness?: number;
        contrast?: number;
        saturation?: number;
    }, options?: {
        parent?: string;
        index?: number;
        name?: string;
    }): string;
    setAdjustment(id: string, adjustment: Record<string, unknown>): void;
    setMask(id: string, mask: unknown): void;
    private insert;
    addRasterFromAsset(asset: AssetRef, options?: {
        parent?: string;
        index?: number;
        name?: string;
    }): string;
    removeNode(id: string): void;
    moveNode(id: string, parent: string, index: number): void;
    setProps(id: string, patch: Record<string, unknown>): void;
    setTransform(id: string, t: number[] | {
        x: number;
        y: number;
        scaleX: number;
        scaleY: number;
        rotation: number;
    }): void;
}
declare class DocumentView extends Events {
    private mirror;
    history: {
        undo: number;
        redo: number;
    };
    snapshotHash: string;
    stateHash: string;
    versions: VersionStatus;
    dirty: boolean;
    warnings: string[];
    apply(state: any): void;
    get selection(): {
        format: string;
        tiles: Record<string, string>;
    } | null;
    /** Asset records (images, fonts) registered in the document. */
    get assets(): any[];
    get root(): NodeView;
    get meta(): Readonly<{
        width: number;
        height: number;
        color: string;
        depth: string;
        format_version: number;
    }>;
    get revision(): number;
    node(id: string): NodeView;
    get nodes(): NodeView[];
}
export declare class ToolSession {
    private doc;
    private samples;
    private ended;
    private ready;
    constructor(doc: DocumentHandle, tool: string, options: BrushOptions);
    input(input: PointerEvent | {
        x: number;
        y: number;
        pressure?: number;
        tiltX?: number;
        tiltY?: number;
        time?: number;
    }): void;
    flush(): Promise<void>;
    end(): Promise<any>;
    cancel(): Promise<void>;
}
export declare class DocumentHandle {
    readonly engine: AltoEngine;
    readonly view: DocumentView;
    activeTool: ToolSession | null;
    /** The folder this document was opened from or last saved to, and its manifest hash. */
    folder: {
        provider: ObjectStorageProvider;
        manifest: string;
    } | null;
    constructor(engine: AltoEngine, state: any);
    apply(s: any): void;
    private textOf;
    readonly text: {
        set: (id: string, content: TextContent) => Promise<void>;
        /** Replaces a UTF-16 range of the text in one undoable edit. */
        replace: (id: string, start: number, end: number, insert: string, intent?: string) => Promise<void>;
        style: (id: string, start: number, end: number, patch: StylePatch) => Promise<void>;
        setTateChuYoko: (id: string, start: number, end: number, on?: boolean) => Promise<void>;
        info: (id: string) => Promise<TextInfo>;
        /** UTF-16 offset nearest to a document point. */
        hitTest: (id: string, x: number, y: number) => Promise<number>;
        caret: (id: string, offset: number) => Promise<TextCaret | null>;
        /** Highlight quads in document coordinates. */
        selectionRects: (id: string, start: number, end: number) => Promise<[number, number][][]>;
        navigate: (id: string, offset: number, motion: TextMotion) => Promise<number>;
    };
    readonly versions: {
        /** Records the current document as a version. Fails when nothing changed unless allowEmpty. */
        commit: (message: string, options?: {
            author?: string;
            allowEmpty?: boolean;
        }) => Promise<CommitView>;
        log: () => Promise<CommitView[]>;
        /** Semantic, layer-level diff. Use "working" for the current document. */
        diff: (from: string, to?: string) => Promise<VersionDiff>;
        status: () => VersionStatus;
        /** Restores a version as one undoable edit; history is kept. */
        restore: (hash: string) => Promise<void>;
        /** Drops versions that cannot be reached from the head. Commit and open keep every version. */
        prune: () => Promise<number>;
    };
    /** Saves the folder form. Content-addressed files are written once; manifest.json last. */
    saveFolder(provider: ObjectStorageProvider, options?: {
        overwrite?: boolean;
    }): Promise<{
        written: number;
        skipped: number;
    }>;
    readonly vectors: {
        set: (id: string, paths: StyledPath[]) => Promise<void>;
        setMask: (id: string, mask: VectorMask | null) => Promise<void>;
        setClipPath: (id: string, path: ClipPath | null) => Promise<void>;
    };
    readonly tools: {
        begin: (tool: "brush" | "eraser", options: BrushOptions) => ToolSession;
    };
    transact<T>(intent: string, callback: (tx: TransactionBuilder) => T): Promise<T>;
    undo(): Promise<void>;
    redo(): Promise<void>;
    command(type: string, payload: any): Promise<void>;
    readonly selection: {
        apply: (spec: {
            kind: "rectangle" | "ellipse" | "lasso" | "all" | "invert" | "clear";
            mode?: "replace" | "add" | "subtract" | "intersect";
            x?: number;
            y?: number;
            width?: number;
            height?: number;
            feather?: number;
            points?: number[][];
        }) => Promise<void>;
        outline: () => Promise<string>;
        clear: () => Promise<void>;
        invert: () => Promise<void>;
        all: () => Promise<void>;
    };
    readonly masks: {
        fromSelection: (layer: NodeId) => Promise<void>;
        addWhite: (layer: NodeId) => Promise<void>;
        remove: (layer: NodeId) => Promise<void>;
    };
    readonly effects: {
        set: (layer: NodeId, effects: Record<string, unknown>[], options?: {
            useSelection?: boolean;
        }) => Promise<void>;
    };
    save(target?: ArchiveStorageProvider | any): Promise<any>;
}
export declare class AltoEngine extends Events {
    canvas: HTMLCanvasElement;
    private worker;
    private sequence;
    private pending;
    private mutation;
    private raf;
    private disposed;
    private releaseLease?;
    private onPageHide;
    private boot;
    private options;
    document: DocumentHandle | null;
    recoveryAvailable: boolean;
    /** Set when the working journal can no longer confirm writes. Edits stay frozen until recovery. */
    storageFailure: AltoError | null;
    private constructor();
    static create(options: {
        canvas: HTMLCanvasElement;
        gpu?: {
            powerPreference?: string;
            memoryBudgetMB?: number;
        };
        workerURL?: URL | string;
    }): Promise<AltoEngine>;
    private acquireLease;
    private start;
    request(t: string, payload?: any, transfer?: Transferable[]): Promise<any>;
    stream(message: any, transfer?: Transferable[]): void;
    mutate<T>(fn: () => Promise<T>): Promise<T>;
    resize(): void;
    createDocument(meta: {
        width: number;
        height: number;
    }): Promise<DocumentHandle>;
    openDocument(source: Blob | ArchiveStorageProvider | any): Promise<DocumentHandle>;
    /** Opens the folder form, reading only objects reachable from the document and its history. */
    openFolder(provider: ObjectStorageProvider): Promise<DocumentHandle>;
    recover(): Promise<DocumentHandle>;
    /** Exports the live runtime without touching the failed journal or marking it saved. */
    exportEmergencyBytes(): Promise<Uint8Array>;
    /** Recreates both workers and replays the confirmed journal after a storage failure. */
    recoverAfterStorageFailure(): Promise<{
        document: DocumentHandle;
        beforeRevision: number | null;
        recoveredRevision: number;
        matchesDisplay: boolean | null;
    }>;
    readonly assets: {
        /** Imports an OpenType/TrueType font (.otf/.ttf/.ttc). Add it with tx.addFont(). */
        importFont: (file: Blob, options?: {
            faceIndex?: number;
        }) => Promise<FontAsset>;
        /** Abandons an imported font or encoded image that was never added with SetAsset. */
        releaseImport: (hash: string) => Promise<boolean>;
        importImage: (file: Blob) => Promise<AssetRef>;
    };
    readonly viewport: {
        version: number;
        center: [number, number];
        zoom: number;
        rotation: number;
        set: (v: {
            center?: [number, number];
            zoom?: number;
            rotation?: number;
        }) => void;
        screenToDocument: (x: number, y: number) => [number, number];
    };
    readonly storage: {
        /** Picks a folder for the `.altoproj` form (File System Access API). */
        pickFolder: () => Promise<ObjectStorageProvider>;
        /** A folder inside the origin private file system. */
        opfsFolder: (name: string) => Promise<ObjectStorageProvider>;
        pickSaveFile: (options: {
            suggestedName: string;
        }) => Promise<any>;
    };
    readonly vectors: {
        parseSvg: (svg: string) => Promise<PathData>;
        toSvg: (data: PathData) => Promise<string>;
    };
    readonly unstable: {
        settle: () => Promise<any>;
        readTile: (x: number, y: number) => Promise<Uint8Array<any>>;
        readTileLevel: (level: number, x: number, y: number) => Promise<Uint8Array<any>>;
        vectorStats: () => Promise<any>;
        /** Small text through the glyph atlas (default) or always through the vector path. */
        setGlyphAtlas: (enabled: boolean) => Promise<any>;
        destroyGpu: () => Promise<any>;
        crash: () => Promise<void>;
        corruptJournalTail: () => Promise<any>;
        /** Fault injection for recovery tests only. */
        injectStorageFault: (position: "beforeAppend" | "afterAppend") => Promise<any>;
        saveBytes: () => Promise<Uint8Array<any>>;
    };
    private restart;
    dispose(): void;
}
export {};
