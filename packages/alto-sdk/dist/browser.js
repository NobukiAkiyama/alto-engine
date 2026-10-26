/**
 * Helpers that use the browser's file pickers and storage. The engine itself never
 * opens a picker; a host that wants one calls these.
 */
import { AltoError } from "./errors.js";
import { FileSystemDirectoryProvider } from "./storage.js";
/** Writes to a file handle from the File System Access API, replacing it on success. */
export function fileHandleTarget(handle) {
    return {
        async write(stream) {
            const writer = await handle.createWritable();
            try {
                await stream.pipeTo(writer);
            }
            catch (e) {
                await writer.abort().catch(() => { });
                throw e;
            }
        },
    };
}
/** Offers the archive as a download named `name`. */
export function downloadTarget(name) {
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
export function blobTarget() {
    let blob = null;
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
export async function pickSaveTarget(options) {
    const picker = window
        .showSaveFilePicker;
    if (!picker)
        return downloadTarget(options.suggestedName);
    const handle = await picker({
        suggestedName: options.suggestedName,
        types: [{ description: "Alto document", accept: { "application/zip": [".alto"] } }],
    });
    return fileHandleTarget(handle);
}
/** Reads a picked or dropped file. */
export function fileSource(file) {
    return { read: async () => (file instanceof Blob ? file : file.getFile()) };
}
/** Picks a folder for the `.altoproj` form (File System Access API). */
export async function pickFolder() {
    const picker = window
        .showDirectoryPicker;
    if (!picker)
        throw new AltoError("STORAGE", "this browser cannot save to folders");
    return new FileSystemDirectoryProvider(await picker({ mode: "readwrite" }));
}
/** A folder inside the origin private file system. */
export async function opfsFolder(name) {
    const root = await navigator.storage.getDirectory();
    return new FileSystemDirectoryProvider(await root.getDirectoryHandle(name, { create: true }));
}
/** Whether this browser can run the engine, and if not, what is missing. */
export async function isSupported() {
    if (typeof HTMLCanvasElement === "undefined" || !("transferControlToOffscreen" in HTMLCanvasElement.prototype))
        return { ok: false, reason: "offscreen-canvas" };
    const gpu = navigator.gpu;
    if (!gpu || !(await gpu.requestAdapter().catch(() => null)))
        return { ok: false, reason: "webgpu" };
    if (!navigator.locks)
        return { ok: false, reason: "web-locks" };
    if (!navigator.storage?.getDirectory)
        return { ok: false, reason: "opfs" };
    return { ok: true };
}
