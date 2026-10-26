/** Recovery journals on this origin, one folder per storage namespace. */
import { AltoError } from "./errors.js";
const PREFIX = "ns-";
const decoder = new TextDecoder();
function nameOf(folder) {
    if (!folder.startsWith(PREFIX))
        return null;
    const hex = folder.slice(PREFIX.length);
    if (!/^(?:[0-9a-f]{2})+$/.test(hex))
        return null;
    return decoder.decode(Uint8Array.from(hex.match(/../g), (h) => parseInt(h, 16)));
}
function folderOf(namespace) {
    return PREFIX + Array.from(new TextEncoder().encode(namespace), (b) => b.toString(16).padStart(2, "0")).join("");
}
async function root() {
    try {
        return await (await navigator.storage.getDirectory()).getDirectoryHandle("alto-engine");
    }
    catch {
        return null;
    }
}
async function measure(dir) {
    let bytes = 0, modified = 0;
    for await (const [, handle] of dir.entries()) {
        if (handle.kind === "directory") {
            const inner = await measure(handle);
            bytes += inner.bytes;
            modified = Math.max(modified, inner.modified);
        }
        else {
            const file = await handle.getFile();
            bytes += file.size;
            modified = Math.max(modified, file.lastModified);
        }
    }
    return { bytes, modified };
}
/** Every namespace with a journal on this origin, its size in bytes and last write time (ms). */
export async function listRecoveryNamespaces() {
    const dir = await root();
    if (!dir)
        return [];
    const out = [];
    for await (const [folder, handle] of dir.entries()) {
        const namespace = handle.kind === "directory" ? nameOf(folder) : null;
        if (namespace !== null)
            out.push({ namespace, ...(await measure(handle)) });
    }
    return out.sort((a, b) => a.namespace.localeCompare(b.namespace));
}
/** Deletes a namespace's journal. Fails with CONFLICT while an engine uses the namespace. */
export async function deleteRecovery(namespace) {
    const dir = await root();
    if (!dir)
        return;
    await navigator.locks.request(`alto-engine:${namespace}`, { ifAvailable: true }, async (lock) => {
        if (!lock)
            throw new AltoError("CONFLICT", `storage namespace ${namespace} is in use`, {
                reason: "namespace-in-use",
                namespace,
            });
        await dir.removeEntry(folderOf(namespace), { recursive: true }).catch((e) => {
            if (e?.name !== "NotFoundError")
                throw new AltoError("STORAGE", String(e));
        });
    });
}
