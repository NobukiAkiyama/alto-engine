/**
 * Helpers that use the browser's file pickers and storage. The engine itself never
 * opens a picker; a host that wants one calls these.
 */
import { AltoError } from "./errors.js";
import { FileSystemDirectoryProvider, type ArchiveSource, type ArchiveTarget, type ObjectStorageProvider } from "./storage.js";

export type { ArchiveSource, ArchiveTarget, ObjectStorageProvider };

/** Writes to a file handle from the File System Access API, replacing it on success. */
export function fileHandleTarget(handle: FileSystemFileHandle): ArchiveTarget {
  return {
    async write(stream) {
      const writer = await handle.createWritable();
      try {
        await stream.pipeTo(writer);
      } catch (e) {
        await writer.abort().catch(() => {});
        throw e;
      }
    },
  };
}
/** Offers the archive as a download named `name`. */
export function downloadTarget(name: string): ArchiveTarget {
  return {
    async write(stream) {
      const blob = await new Response(stream).blob();
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = name;
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 1000);
    },
  };
}
/** Keeps the archive in memory; read `blob` after `save` resolves. */
export function blobTarget(): ArchiveTarget & { readonly blob: Blob | null } {
  let blob: Blob | null = null;
  return {
    get blob() {
      return blob;
    },
    async write(stream) {
      blob = await new Response(stream).blob();
    },
  };
}
/** Asks where to save with the browser's save dialog; falls back to a download without one. */
export async function pickSaveTarget(options: { suggestedName: string }): Promise<ArchiveTarget> {
  const picker = (window as unknown as { showSaveFilePicker?: (o: unknown) => Promise<FileSystemFileHandle> })
    .showSaveFilePicker;
  if (!picker) return downloadTarget(options.suggestedName);
  const handle = await picker({
    suggestedName: options.suggestedName,
    types: [{ description: "Alto document", accept: { "application/zip": [".alto"] } }],
  });
  return fileHandleTarget(handle);
}
/** Reads a picked or dropped file. */
export function fileSource(file: Blob | FileSystemFileHandle): ArchiveSource {
  return { read: async () => (file instanceof Blob ? file : file.getFile()) };
}
/** Picks a folder for the `.altoproj` form (File System Access API). */
export async function pickFolder(): Promise<ObjectStorageProvider> {
  const picker = (window as unknown as { showDirectoryPicker?: (o: unknown) => Promise<FileSystemDirectoryHandle> })
    .showDirectoryPicker;
  if (!picker) throw new AltoError("STORAGE", "this browser cannot save to folders");
  return new FileSystemDirectoryProvider(await picker({ mode: "readwrite" }));
}
/** A folder inside the origin private file system. */
export async function opfsFolder(name: string): Promise<ObjectStorageProvider> {
  const root = await navigator.storage.getDirectory();
  return new FileSystemDirectoryProvider(await root.getDirectoryHandle(name, { create: true }));
}

/** Whether this browser can run the engine, and if not, what is missing. */
export async function isSupported(): Promise<{ ok: boolean; reason?: "webgpu" | "offscreen-canvas" | "web-locks" | "opfs" }> {
  if (typeof HTMLCanvasElement === "undefined" || !("transferControlToOffscreen" in HTMLCanvasElement.prototype))
    return { ok: false, reason: "offscreen-canvas" };
  const gpu = (navigator as Navigator & { gpu?: { requestAdapter(): Promise<unknown> } }).gpu;
  if (!gpu || !(await gpu.requestAdapter().catch(() => null))) return { ok: false, reason: "webgpu" };
  if (!navigator.locks) return { ok: false, reason: "web-locks" };
  if (!navigator.storage?.getDirectory) return { ok: false, reason: "opfs" };
  return { ok: true };
}
