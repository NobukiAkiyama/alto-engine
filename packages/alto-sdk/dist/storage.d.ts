/** Where `DocumentHandle.save` writes a `.alto` archive. */
export interface ArchiveTarget {
    write(stream: ReadableStream<Uint8Array>): Promise<void>;
}
/** Where `Engine.open` reads a `.alto` archive from. */
export interface ArchiveSource {
    read(): Promise<Blob>;
}
/**
 * Object-level storage for the folder form (`.altoproj/`). Content-addressed files
 * (objects/, snapshots/, commits/) are addressed by package path and never rewritten.
 * Refs (manifest.json and the readable mirrors) are replaced; `expectHash` is the
 * SHA-256 hex of the ref the caller based its write on (optimistic lock, "" = absent).
 */
export interface ObjectStorageProvider {
    readonly kind: string;
    /** True only when all clients of this storage participate in the same lock. */
    readonly gcCapabilities?: {
        readonly exclusive: true;
        readonly atomicManifest: true;
        readonly deleteObjects: true;
    };
    exclusive?<T>(run: (storage: ExclusiveObjectStorage) => Promise<T>): Promise<T>;
    /** Keeps real GC from running until the returned function's promise settles. */
    holdObjects?(): Promise<() => Promise<void>>;
    /** Runs `run` only if no other document holds this storage's objects, else `BUSY`. */
    whileNotHeld?<T>(run: () => Promise<T>): Promise<T>;
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
/** A storage view valid only for the duration of its exclusive callback. */
export interface ExclusiveObjectStorage extends ObjectStorageProvider {
    deleteObjects(paths: string[]): Promise<void>;
}
export declare function sha256Hex(data: Uint8Array): Promise<string>;
export declare class MemoryObjectProvider implements ObjectStorageProvider {
    #private;
    readonly kind = "memory";
    readonly gcCapabilities: {
        readonly exclusive: true;
        readonly atomicManifest: true;
        readonly deleteObjects: true;
    };
    exclusive<T>(run: (storage: ExclusiveObjectStorage) => Promise<T>): Promise<T>;
    getObject(path: string): Promise<Uint8Array<ArrayBufferLike> | null>;
    readRef(name: string): Promise<Uint8Array<ArrayBufferLike> | null>;
    list(prefix: string): Promise<string[]>;
    putObjects(entries: {
        path: string;
        data: Uint8Array;
    }[]): Promise<{
        written: number;
        skipped: number;
    }>;
    writeRef(name: string, data: Uint8Array, expectHash?: string): Promise<void>;
}
/**
 * A directory from the File System Access API. Writes replace atomically on close.
 *
 * A folder the user picked can be changed by other origins and native programs, and
 * nothing a page can do excludes them, so this provider offers no garbage collection.
 * Given a directory inside OPFS, its writes still take the same lock as
 * {@link OpfsFolderProvider}, so that every writer of an OPFS folder participates.
 */
export declare class FileSystemDirectoryProvider implements ObjectStorageProvider {
    #private;
    readonly handle: FileSystemDirectoryHandle;
    readonly kind: string;
    constructor(handle: FileSystemDirectoryHandle);
    private dir;
    private file;
    private participate;
    getObject(path: string): Promise<Uint8Array | null>;
    putObjects(entries: {
        path: string;
        data: Uint8Array;
    }[]): Promise<{
        written: number;
        skipped: number;
    }>;
    private exists;
    list(prefix: string): Promise<string[]>;
    readRef(name: string): Promise<Uint8Array<ArrayBufferLike> | null>;
    writeRef(name: string, data: Uint8Array, expectHash?: string): Promise<void>;
}
/**
 * A folder inside the origin private file system, with garbage collection.
 *
 * Only code of this origin can reach OPFS, and every context of an origin shares its Web
 * Locks, so one lock per folder excludes all SDK writers: other tabs, workers and
 * {@link FileSystemDirectoryProvider} on the same directory. The browser releases the
 * lock if its holder dies, and a dead context writes nothing. A script that edits the
 * directory without the SDK is outside this guarantee.
 */
export declare class OpfsFolderProvider extends FileSystemDirectoryProvider {
    #private;
    readonly kind: string;
    readonly gcCapabilities: {
        readonly exclusive: true;
        readonly atomicManifest: true;
        readonly deleteObjects: true;
    };
    private constructor();
    /** Refuses a directory outside OPFS, where this provider's guarantee would not hold. */
    static open(handle: FileSystemDirectoryHandle, options?: {
        waitMs?: number;
    }): Promise<OpfsFolderProvider>;
    holdObjects(): Promise<() => Promise<void>>;
    whileNotHeld<T>(run: () => Promise<T>): Promise<T>;
    exclusive<T>(run: (storage: ExclusiveObjectStorage) => Promise<T>): Promise<T>;
    getObject(path: string): Promise<Uint8Array<ArrayBufferLike> | null>;
    readRef(name: string): Promise<Uint8Array<ArrayBufferLike> | null>;
    list(prefix: string): Promise<string[]>;
    putObjects(entries: {
        path: string;
        data: Uint8Array;
    }[]): Promise<{
        written: number;
        skipped: number;
    }>;
    writeRef(name: string, data: Uint8Array, expectHash?: string): Promise<void>;
}
