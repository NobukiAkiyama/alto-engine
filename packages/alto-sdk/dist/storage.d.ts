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
