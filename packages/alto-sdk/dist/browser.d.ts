import { type ArchiveSource, type ArchiveTarget, type ObjectStorageProvider } from "./storage.js";
export type { ArchiveSource, ArchiveTarget, ObjectStorageProvider };
/** Writes to a file handle from the File System Access API, replacing it on success. */
export declare function fileHandleTarget(handle: FileSystemFileHandle): ArchiveTarget;
/** Offers the archive as a download named `name`. */
export declare function downloadTarget(name: string): ArchiveTarget;
/** Keeps the archive in memory; read `blob` after `save` resolves. */
export declare function blobTarget(): ArchiveTarget & {
    readonly blob: Blob | null;
};
/** Asks where to save with the browser's save dialog; falls back to a download without one. */
export declare function pickSaveTarget(options: {
    suggestedName: string;
}): Promise<ArchiveTarget>;
/** Reads a picked or dropped file. */
export declare function fileSource(file: Blob | FileSystemFileHandle): ArchiveSource;
/** Picks a folder for the `.altoproj` form (File System Access API). */
export declare function pickFolder(): Promise<ObjectStorageProvider>;
/** A folder inside the origin private file system. */
export declare function opfsFolder(name: string): Promise<ObjectStorageProvider>;
/** Whether this browser can run the engine, and if not, what is missing. */
export declare function isSupported(): Promise<{
    ok: boolean;
    reason?: "webgpu" | "offscreen-canvas" | "web-locks" | "opfs";
}>;
