export type NodeId = string;
export type BlendMode = 'normal' | 'multiply' | 'screen';
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
        mask: unknown;
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
export interface ObjectStorageProvider {
    readonly kind: string;
    getObject(hash: string): Promise<Uint8Array | null>;
    putObjects(entries: {
        hash: string;
        data: Uint8Array;
    }[]): Promise<void>;
    readRef(name: string): Promise<Uint8Array | null>;
    writeRef(name: string, data: Uint8Array, expectHash?: string): Promise<void>;
}
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
    }): `${string}-${string}-${string}-${string}-${string}`;
    addGroup(options?: {
        parent?: string;
        index?: number;
        name?: string;
        isolated?: boolean;
    }): `${string}-${string}-${string}-${string}-${string}`;
    private insert;
    addRasterFromAsset(asset: AssetRef, options?: {
        parent?: string;
        index?: number;
        name?: string;
    }): `${string}-${string}-${string}-${string}-${string}`;
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
    dirty: boolean;
    warnings: string[];
    apply(state: any): void;
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
    constructor(engine: AltoEngine, state: any);
    apply(s: any): void;
    readonly tools: {
        begin: (tool: "brush" | "eraser", options: BrushOptions) => ToolSession;
    };
    transact<T>(intent: string, callback: (tx: TransactionBuilder) => T): Promise<T>;
    undo(): Promise<void>;
    redo(): Promise<void>;
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
    private boot;
    private options;
    document: DocumentHandle | null;
    recoveryAvailable: boolean;
    private constructor();
    static create(options: {
        canvas: HTMLCanvasElement;
        gpu?: {
            powerPreference?: string;
            memoryBudgetMB?: number;
        };
        workerURL?: URL | string;
    }): Promise<AltoEngine>;
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
    recover(): Promise<DocumentHandle>;
    readonly assets: {
        importImage: (file: Blob) => Promise<AssetRef>;
    };
    readonly viewport: {
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
        pickSaveFile: (options: {
            suggestedName: string;
        }) => Promise<any>;
    };
    readonly unstable: {
        readTile: (x: number, y: number) => Promise<Uint8Array<any>>;
        destroyGpu: () => Promise<any>;
        crash: () => Promise<void>;
        saveBytes: () => Promise<Uint8Array<any>>;
    };
    private restart;
    dispose(): void;
}
export {};
