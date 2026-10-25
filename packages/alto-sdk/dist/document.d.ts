import { type Intent } from "./intents.js";
import { type DocumentChange, type DocumentSnapshot } from "./snapshot.js";
import { type ArchiveTarget, type ObjectStorageProvider } from "./storage.js";
import { ToolSession, type BrushOptions } from "./tools.js";
import { Transaction } from "./transaction.js";
import type { ClipPath, CommitView, Effect, NodeId, Rect, StylePatch, StyledPath, TextCaret, TextContent, TextInfo, TextMotion, TileRef, VectorMask, VersionDiff, VersionStatus } from "./types.js";
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
export declare class DocumentHandle {
    private readonly core;
    private readonly store;
    private readonly listeners;
    private tool;
    private savedFolder;
    /** The current state. Unchanged until the next edit, so it is safe to keep. */
    getSnapshot(): DocumentSnapshot;
    /** Calls `listener` after each change with what changed. Returns an unsubscribe function. */
    subscribe(listener: (change: DocumentChange) => void): () => void;
    /** The stroke in progress, if any. */
    get activeTool(): ToolSession | null;
    /** The folder this document was opened from or last saved to. */
    get folder(): ObjectStorageProvider | null;
    private command;
    private edit;
    /**
     * Applies one edit. `build` must be synchronous; its return value is returned.
     * Resolves when the edit is applied; `durable()` waits for the recovery journal.
     */
    transact<T>(intent: Intent, build: (tx: Transaction) => T): Promise<T>;
    /** Undoes the last edit. */
    undo(): Promise<void>;
    /** Redoes the last undone edit. */
    redo(): Promise<void>;
    /**
     * Resolves once every edit made so far is in the recovery journal. Edits resolve
     * when applied; this rejects with STORAGE if the journal could not be written.
     */
    durable(): Promise<void>;
    /** The hash of the snapshot a save would write. Computed on request, not per edit. */
    snapshotHash(): Promise<string>;
    /** A raster layer's full tile map. */
    nodeTiles(id: NodeId): Promise<TileRef>;
    /** Brush and eraser strokes. */
    readonly tools: {
        /** Starts a stroke. Only one stroke runs at a time. */
        begin: (tool: "brush" | "eraser", options: BrushOptions) => ToolSession;
    };
    private textOf;
    /** Text layer editing and layout queries. Offsets are UTF-16 code units. */
    readonly text: {
        set: (id: NodeId, content: TextContent) => Promise<void>;
        /** Replaces a UTF-16 range of the text in one undoable edit. */
        replace: (id: NodeId, start: number, end: number, insert: string, intent?: Intent) => Promise<void>;
        style: (id: NodeId, start: number, end: number, patch: StylePatch) => Promise<void>;
        setTateChuYoko: (id: NodeId, start: number, end: number, on?: boolean) => Promise<void>;
        info: (id: NodeId) => Promise<TextInfo>;
        /** UTF-16 offset nearest to a document point. */
        hitTest: (id: NodeId, x: number, y: number) => Promise<number>;
        caret: (id: NodeId, offset: number) => Promise<TextCaret | null>;
        /** Highlight quads in document coordinates. */
        selectionRects: (id: NodeId, start: number, end: number) => Promise<[number, number][][]>;
        navigate: (id: NodeId, offset: number, motion: TextMotion) => Promise<number>;
    };
    /** Vector paths, vector masks and clipping paths. */
    readonly vectors: {
        set: (id: NodeId, paths: StyledPath[]) => Promise<void>;
        setMask: (id: NodeId, mask: VectorMask | null) => Promise<void>;
        setClipPath: (id: NodeId, path: ClipPath | null) => Promise<void>;
    };
    /** The document selection (an R8 mask). */
    readonly selection: {
        apply: (spec: SelectionSpec) => Promise<void>;
        /** The selection's outline as SVG path data in document coordinates. */
        outline: () => Promise<string>;
        clear: () => Promise<void>;
        invert: () => Promise<void>;
        all: () => Promise<void>;
    };
    /** A layer's pixel mask. */
    readonly masks: {
        fromSelection: (layer: NodeId) => Promise<void>;
        addWhite: (layer: NodeId) => Promise<void>;
        remove: (layer: NodeId) => Promise<void>;
    };
    /** A layer's non-destructive effects. */
    readonly effects: {
        /** Replaces a layer's effects. With `useSelection` (default) each is limited to the selection. */
        set: (layer: NodeId, effects: Effect[], options?: {
            useSelection?: boolean;
        }) => Promise<void>;
    };
    /** Version commits, diffs and restores. */
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
        /** Drops versions that cannot be reached from the head. Returns how many. */
        prune: () => Promise<number>;
    };
    /** Writes the document as a `.alto` archive. On success the document is no longer dirty. */
    save(target: ArchiveTarget): Promise<void>;
    /**
     * Saves the folder form. Content-addressed files are written once; manifest.json
     * last. A folder holding another document is refused unless `overwrite`.
     */
    saveFolder(provider: ObjectStorageProvider, options?: {
        overwrite?: boolean;
    }): Promise<{
        written: number;
        skipped: number;
    }>;
    /** A cancelled or failed save no longer needs the Worker's copy of the package. */
    private releaseSave;
}
