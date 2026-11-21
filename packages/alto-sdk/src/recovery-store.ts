/** Recovery journals on this origin, one folder per storage namespace. */
import { AltoError } from "./errors.js";

const PREFIX = "ns-";
const decoder = new TextDecoder();
function nameOf(folder: string): string | null {
  if (!folder.startsWith(PREFIX)) return null;
  const hex = folder.slice(PREFIX.length);
  if (!/^(?:[0-9a-f]{2})+$/.test(hex)) return null;
  return decoder.decode(Uint8Array.from(hex.match(/../g)!, (h) => parseInt(h, 16)));
}
function folderOf(namespace: string): string {
  return PREFIX + Array.from(new TextEncoder().encode(namespace), (b) => b.toString(16).padStart(2, "0")).join("");
}
type Entries = { entries(): AsyncIterable<[string, FileSystemHandle]> };
async function root(): Promise<FileSystemDirectoryHandle | null> {
  try {
    return await (await navigator.storage.getDirectory()).getDirectoryHandle("alto-engine");
  } catch {
    return null;
  }
}
async function measure(dir: FileSystemDirectoryHandle, skipLabel = false): Promise<{ bytes: number; modified: number }> {
  let bytes = 0,
    modified = 0;
  for await (const [name, handle] of (dir as unknown as Entries).entries()) {
    if (skipLabel && name === "label.json") continue;
    try {
      if (handle.kind === "directory") {
        const inner = await measure(handle as FileSystemDirectoryHandle);
        bytes += inner.bytes;
        modified = Math.max(modified, inner.modified);
      } else {
        const file = await (handle as FileSystemFileHandle).getFile();
        bytes += file.size;
        modified = Math.max(modified, file.lastModified);
      }
    } catch (error) {
      // Checkpoint compaction and atomic writes can retire a file between
      // enumeration and getFile. The remaining entries are still listable.
      if (!(error instanceof DOMException) || error.name !== "NotFoundError") throw error;
    }
  }
  return { bytes, modified };
}

interface RecoveryLabel {
  label: string;
  width: number;
  height: number;
  updated: number;
}

/** A recovery journal and its optional document metadata. */
export interface RecoveryNamespace {
  namespace: string;
  bytes: number;
  modified: number;
  label?: string;
  width?: number;
  height?: number;
}

export function validRecoveryLabel(label: unknown): label is string {
  return typeof label === "string" && label.length > 0 && label.length <= 512 && [...label].length <= 256;
}

async function readLabel(dir: FileSystemDirectoryHandle): Promise<RecoveryLabel | undefined> {
  try {
    const file = await (await dir.getFileHandle("label.json")).getFile();
    if (file.size > 4096) return;
    const data = JSON.parse(await file.text());
    if (validRecoveryLabel(data?.label)
      && Number.isSafeInteger(data.width) && data.width > 0
      && Number.isSafeInteger(data.height) && data.height > 0
      && Number.isSafeInteger(data.updated) && data.updated >= 0) {
      return { label: data.label, width: data.width, height: data.height, updated: data.updated };
    }
  } catch {
    // Optional metadata can be missing or unreadable.
  }
}

export async function readRecoveryLabel(namespace: string): Promise<string | undefined> {
  try {
    const dir = await (await root())?.getDirectoryHandle(folderOf(namespace));
    return dir && (await readLabel(dir))?.label;
  } catch {
    return undefined;
  }
}

export async function writeRecoveryLabel(namespace: string, label: string, width: number, height: number): Promise<boolean> {
  try {
    const dir = await (await root())?.getDirectoryHandle(folderOf(namespace));
    if (!dir) return false;
    const file = await dir.getFileHandle("label.json", { create: true });
    const writer = await file.createWritable();
    try {
      await writer.write(JSON.stringify({ label, width, height, updated: Date.now() }));
      await writer.close();
      return true;
    } catch {
      await writer.abort().catch(() => {});
    }
  } catch {
    // A metadata failure must not affect the journal.
  }
  return false;
}

/** Every namespace with a journal on this origin, its size in bytes and last write time (ms). */
export async function listRecoveryNamespaces(): Promise<RecoveryNamespace[]> {
  const dir = await root();
  if (!dir) return [];
  const out: RecoveryNamespace[] = [];
  for await (const [folderName, handle] of (dir as unknown as Entries).entries()) {
    const namespace = handle.kind === "directory" ? nameOf(folderName) : null;
    if (namespace === null) continue;
    const folder = handle as FileSystemDirectoryHandle;
    const row = { namespace, ...(await measure(folder, true)) };
    const metadata = await readLabel(folder);
    out.push(metadata ? { ...row, label: metadata.label, width: metadata.width, height: metadata.height } : row);
  }
  return out.sort((a, b) => a.namespace.localeCompare(b.namespace));
}

/** Deletes a namespace's journal. Fails with CONFLICT while an engine uses the namespace. */
export async function deleteRecovery(namespace: string): Promise<void> {
  const dir = await root();
  if (!dir) return;
  await navigator.locks.request(`alto-engine:${namespace}`, { ifAvailable: true }, async (lock) => {
    if (!lock)
      throw new AltoError("CONFLICT", `storage namespace ${namespace} is in use`, {
        reason: "namespace-in-use",
        namespace,
      });
    await dir.removeEntry(folderOf(namespace), { recursive: true }).catch((e: { name?: string }) => {
      if (e?.name !== "NotFoundError") throw new AltoError("STORAGE", String(e));
    });
  });
}
