import { DocumentHandle } from "./document.js";
import { AltoError } from "./errors.js";
import { type ArchiveSource, type ObjectStorageProvider } from "./storage.js";
import type { FontAsset, ImageAsset, PathData } from "./types.js";
import { ViewportControl, type Viewport } from "./viewport.js";
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
    gpu?: {
        powerPreference?: "low-power" | "high-performance";
        memoryBudgetMB?: number;
    };
    /** Where `worker.js` and the WASM files are served. Must be the page's origin. */
    assets?: {
        baseURL?: string | URL;
    };
    /** Upper bound for the device pixel ratio the canvas renders at (default 2). */
    maxPixelRatio?: number;
}
export type EngineState = "starting" | "ready" | "restarting" | "storage-failed" | "failed" | "disposed";
export interface EngineStatus {
    readonly state: EngineState;
    /** Why the engine is in `storage-failed` or `failed`. */
    readonly error: AltoError | null;
}
export interface EngineEvents {
    status: EngineStatus;
    /** The open document changed to another one (open, create, restore) or closed. */
    document: DocumentHandle | null;
    viewport: Viewport;
    saveState: "saving" | "saved" | "error";
    /** A recovery journal from an earlier session can be restored. */
    recoveryAvailable: {
        records: number;
    };
    /** A failure no call is waiting for, such as a stroke's input failing. */
    error: AltoError;
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
    readonly status: EngineStatus;
    readonly document: DocumentHandle | null;
    readonly viewport: ViewportControl;
    readonly recovery: {
        /** A journal from an earlier session is waiting. */
        readonly available: boolean;
        restore(): Promise<DocumentHandle>;
        /** Deletes the waiting journal, so a new document can start. */
        discard(): Promise<void>;
        /** The live document as a `.alto` archive, for use when storage has failed. */
        exportEmergency(): Promise<Blob>;
        /** Restarts the Worker and replays the confirmed journal after a storage failure. */
        recoverAfterStorageFailure(): Promise<RecoveryReport>;
    };
    readonly assets: {
        importImage(file: Blob): Promise<ImageAsset>;
        /** An OpenType/TrueType font (.otf/.ttf/.ttc). Add it with `Transaction.addFont`. */
        importFont(file: Blob, options?: {
            faceIndex?: number;
        }): Promise<FontAsset>;
        /** Drops an imported image or font that no edit used. */
        releaseImport(asset: ImageAsset | FontAsset): Promise<boolean>;
    };
    readonly vectors: {
        parseSvg(svg: string): Promise<PathData>;
        toSvg(data: PathData): Promise<string>;
    };
    createDocument(meta: {
        width: number;
        height: number;
    }): Promise<DocumentHandle>;
    open(source: Blob | ArchiveSource): Promise<DocumentHandle>;
    /** Opens the folder form, reading only objects reachable from the document and its history. */
    openFolder(provider: ObjectStorageProvider): Promise<DocumentHandle>;
    /** Retries a failed Worker without discarding the confirmed journal. */
    retry(): Promise<void>;
    /** Waits briefly for journal writes, then stops the Worker and removes the canvas. */
    dispose(): Promise<void>;
    on<K extends keyof EngineEvents>(name: K, listener: (value: EngineEvents[K]) => void): () => void;
}
/** Starts an engine in `options.container`. */
export declare function createEngine(options: EngineOptions): Promise<Engine>;
