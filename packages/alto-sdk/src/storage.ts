import { AltoError } from "./errors.js";

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
  readonly gcCapabilities?: { exclusive: true; atomicManifest: true; deleteObjects: true };
  exclusive?<T>(run: (storage: ExclusiveObjectStorage) => Promise<T>): Promise<T>;
  getObject(path: string): Promise<Uint8Array | null>;
  /** Idempotent: existing paths are skipped. */
  putObjects(entries: { path: string; data: Uint8Array }[]): Promise<{ written: number; skipped: number }>;
  list(prefix: string): Promise<string[]>;
  readRef(name: string): Promise<Uint8Array | null>;
  writeRef(name: string, data: Uint8Array, expectHash?: string): Promise<void>;
}
/** A storage view valid only for the duration of its exclusive callback. */
export interface ExclusiveObjectStorage extends ObjectStorageProvider {
  deleteObjects(paths: string[]): Promise<void>;
}
export async function sha256Hex(data: Uint8Array): Promise<string> {
  const d = await crypto.subtle.digest("SHA-256", data as BufferSource);
  return Array.from(new Uint8Array(d), (b) => b.toString(16).padStart(2, "0")).join("");
}
/** @internal Content-addressed folder paths, written once. */
export const immutable = (p: string) => p.startsWith("objects/") || p.startsWith("snapshots/") || p.startsWith("commits/");
function validPath(p: string) {
  return !!p && !p.startsWith("/") && p.split("/").every((s) => s && s !== "." && s !== "..");
}
export class MemoryObjectProvider implements ObjectStorageProvider {
  readonly kind = "memory";
  readonly gcCapabilities = { exclusive: true, atomicManifest: true, deleteObjects: true } as const;
  readonly #files = new Map<string, Uint8Array>();
  #tail: Promise<unknown> = Promise.resolve();
  exclusive<T>(run: (storage: ExclusiveObjectStorage) => Promise<T>): Promise<T> {
    const result = this.#tail.then(async () => {
      let active = true;
      const guard = () => { if (!active) throw new AltoError("CONFLICT", "storage lease expired"); };
      const view: ExclusiveObjectStorage = {
        kind: this.kind,
        getObject: async (path) => { guard(); return this.#files.get(path)?.slice() ?? null; },
        readRef: async (path) => { guard(); return this.#files.get(path)?.slice() ?? null; },
        list: async (prefix) => { guard(); return [...this.#files.keys()].filter((k) => k.startsWith(prefix)).sort(); },
        putObjects: async (entries) => {
          guard(); let written = 0, skipped = 0;
          for (const { path, data } of entries) {
            if (!validPath(path) || !immutable(path)) throw new AltoError("INVALID_ARGUMENT", "invalid object path " + path);
            const previous = this.#files.get(path);
            if (previous) {
              if (previous.length !== data.length || previous.some((b, i) => b !== data[i])) throw new AltoError("CORRUPT_DATA", "immutable bytes differ: " + path);
              skipped++;
            } else { this.#files.set(path, data.slice()); written++; }
          }
          return { written, skipped };
        },
        writeRef: async (name, data, expectHash) => {
          guard(); if (!validPath(name) || immutable(name)) throw new AltoError("INVALID_ARGUMENT", "invalid reference path");
          const current = this.#files.get(name);
          if (expectHash !== undefined && (current ? await sha256Hex(current) : "") !== expectHash)
            throw new AltoError("CONFLICT", name + " changed elsewhere", { reason: "folder-changed" });
          guard(); this.#files.set(name, data.slice());
        },
        deleteObjects: async (paths) => { guard();
          if (paths.some((p) => !validPath(p) || !immutable(p))) throw new AltoError("INVALID_ARGUMENT", "invalid GC path");
          for (const path of paths) this.#files.delete(path);
        },
      };
      try { return await run(view); } finally { active = false; }
    });
    this.#tail = result.catch(() => {}); return result;
  }
  getObject(path: string) { return this.exclusive((s) => s.getObject(path)); }
  readRef(name: string) { return this.exclusive((s) => s.readRef(name)); }
  list(prefix: string) { return this.exclusive((s) => s.list(prefix)); }
  putObjects(entries: { path: string; data: Uint8Array }[]) { return this.exclusive((s) => s.putObjects(entries)); }
  writeRef(name: string, data: Uint8Array, expectHash?: string) { return this.exclusive((s) => s.writeRef(name, data, expectHash)); }
}
/** A directory from the File System Access API or OPFS. Writes replace atomically on close. */
export class FileSystemDirectoryProvider implements ObjectStorageProvider {
  readonly kind = "fs-access-dir";
  constructor(readonly handle: FileSystemDirectoryHandle) {}
  private async dir(parts: string[], create: boolean) {
    let d = this.handle;
    for (const p of parts) d = await d.getDirectoryHandle(p, { create });
    return d;
  }
  private async file(path: string, create: boolean) {
    if (!validPath(path)) throw new AltoError("INVALID_ARGUMENT", "invalid path " + path);
    const parts = path.split("/");
    const name = parts.pop()!;
    return (await this.dir(parts, create)).getFileHandle(name, { create });
  }
  async getObject(path: string) {
    try {
      const f = await (await this.file(path, false)).getFile();
      return new Uint8Array(await f.arrayBuffer());
    } catch (e) {
      const name = (e as { name?: string } | null)?.name;
      if (name === "NotFoundError" || name === "TypeError") return null;
      throw new AltoError("STORAGE", String(e));
    }
  }
  private async write(path: string, data: Uint8Array) {
    try {
      const w = await (await this.file(path, true)).createWritable();
      try { await w.write(data as BufferSource); await w.close(); } catch (e) { await w.abort(); throw e; }
    } catch (e) {
      throw e instanceof AltoError ? e : new AltoError("STORAGE", String(e));
    }
  }
  async putObjects(entries: { path: string; data: Uint8Array }[]) {
    let written = 0, skipped = 0;
    for (const e of entries) {
      if (await this.exists(e.path)) skipped++;
      else { await this.write(e.path, e.data); written++; }
    }
    return { written, skipped };
  }
  private async exists(path: string) {
    try { await this.file(path, false); return true; } catch { return false; }
  }
  async list(prefix: string) {
    const out: string[] = [];
    const parts = prefix.split("/").filter(Boolean);
    let root: FileSystemDirectoryHandle;
    try { root = await this.dir(parts, false); } catch { return out; }
    const walk = async (d: FileSystemDirectoryHandle, base: string) => {
      for await (const [name, h] of (d as unknown as { entries(): AsyncIterable<[string, FileSystemHandle]> }).entries()) {
        if (name.startsWith(".")) continue;
        if (h.kind === "directory") await walk(h as FileSystemDirectoryHandle, base + name + "/");
        else out.push(base + name);
      }
    };
    await walk(root, parts.length ? parts.join("/") + "/" : "");
    return out.sort();
  }
  async readRef(name: string) { return this.getObject(name); }
  async writeRef(name: string, data: Uint8Array, expectHash?: string) {
    if (expectHash !== undefined) {
      const cur = await this.getObject(name);
      if ((cur ? await sha256Hex(cur) : "") !== expectHash) throw new AltoError("CONFLICT", name + " changed elsewhere", { reason: "folder-changed" });
    }
    await this.write(name, data);
  }
}
/** @internal Worker <-> main framing for package entries. */
export function frameEntries(entries: Map<string, Uint8Array>): Uint8Array {
  const enc = new TextEncoder();
  const names = [...entries.keys()].map((k) => enc.encode(k));
  let size = 4;
  let i = 0;
  for (const v of entries.values()) size += 8 + names[i++].length + v.length;
  const out = new Uint8Array(size);
  const view = new DataView(out.buffer);
  view.setUint32(0, entries.size, true);
  let p = 4;
  i = 0;
  for (const v of entries.values()) {
    const n = names[i++];
    view.setUint32(p, n.length, true); out.set(n, p + 4); p += 4 + n.length;
    view.setUint32(p, v.length, true); out.set(v, p + 4); p += 4 + v.length;
  }
  return out;
}
/** @internal */
export function unframeEntries(b: Uint8Array): Map<string, Uint8Array> {
  const view = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const dec = new TextDecoder();
  const out = new Map<string, Uint8Array>();
  let p = 4;
  for (let i = 0, n = view.getUint32(0, true); i < n; i++) {
    const l = view.getUint32(p, true);
    const name = dec.decode(b.subarray(p + 4, p + 4 + l)); p += 4 + l;
    const d = view.getUint32(p, true);
    out.set(name, b.subarray(p + 4, p + 4 + d)); p += 4 + d;
  }
  return out;
}
