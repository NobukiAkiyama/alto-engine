import assert from "node:assert/strict";
import { test } from "node:test";

// An in-memory OPFS: enough of FileSystemDirectoryHandle, FileSystemFileHandle and
// FileSystemSyncAccessHandle for storage-core. `failReads` makes getFile() on a file
// of that name throw, as a read of an existing file can.
class FakeFile {
  constructor(name, fs) {
    this.kind = "file";
    this.name = name;
    this.fs = fs;
    this.data = new Uint8Array(0);
  }
  async getFile() {
    if (this.fs.failReads.has(this.name))
      throw new DOMException(`could not read ${this.name}`, "NotReadableError");
    const bytes = this.data.slice();
    return { size: bytes.length, arrayBuffer: async () => bytes.buffer };
  }
  async createSyncAccessHandle() {
    return {
      truncate: (n) => {
        const out = new Uint8Array(n);
        out.set(this.data.subarray(0, n));
        this.data = out;
      },
      write: (bytes, { at }) => {
        if (at + bytes.length > this.data.length) {
          const out = new Uint8Array(at + bytes.length);
          out.set(this.data);
          this.data = out;
        }
        this.data.set(bytes, at);
        return bytes.length;
      },
      flush() {},
      close() {},
    };
  }
}
class FakeDirectory {
  constructor(name, fs) {
    this.kind = "directory";
    this.fs = fs;
    this.children = new Map();
  }
  get(name, Kind, create) {
    let child = this.children.get(name);
    if (!child) {
      if (!create) throw new DOMException(`${name} not found`, "NotFoundError");
      child = new Kind(name, this.fs);
      this.children.set(name, child);
    }
    if (!(child instanceof Kind)) throw new DOMException(`${name} has another kind`, "TypeMismatchError");
    return child;
  }
  async getFileHandle(name, { create = false } = {}) {
    return this.get(name, FakeFile, create);
  }
  async getDirectoryHandle(name, { create = false } = {}) {
    return this.get(name, FakeDirectory, create);
  }
  async removeEntry(name) {
    if (!this.children.delete(name)) throw new DOMException(`${name} not found`, "NotFoundError");
  }
  async *entries() {
    yield* this.children;
  }
}
const fs = { failReads: new Set() };
const root = new FakeDirectory("", fs);
Object.defineProperty(globalThis, "navigator", {
  value: { storage: { getDirectory: async () => root } },
  configurable: true,
});

// Each session imports its own copy of the module, as each Storage Worker does.
let sessions = 0;
const openStore = async () => {
  const { handle } = await import(`../dist/storage-core.js?session=${++sessions}`);
  await handle({ t: "configure", namespace: "test" });
  return handle;
};

const encoder = new TextEncoder();
const decoder = new TextDecoder();
function frame(entries) {
  const parts = [];
  const count = new Uint8Array(4);
  new DataView(count.buffer).setUint32(0, entries.size, true);
  parts.push(count);
  for (const [name, data] of entries) {
    const n = encoder.encode(name);
    const head = new Uint8Array(4);
    new DataView(head.buffer).setUint32(0, n.length, true);
    const tail = new Uint8Array(4);
    new DataView(tail.buffer).setUint32(0, data.length, true);
    parts.push(head, n, tail, data);
  }
  const out = new Uint8Array(parts.reduce((s, p) => s + p.length, 0));
  let at = 0;
  for (const p of parts) out.set(p, (at += p.length) - p.length);
  return out;
}
const object = (n) => `objects/${n}${n}/${String(n).repeat(64)}`;
/** A checkpoint of document version `version` holding objects 1..version. */
function checkpointBytes(version) {
  const objects = Array.from({ length: version }, (_, i) => object(i + 1));
  const entries = new Map([
    ["checkpoint.json", encoder.encode(JSON.stringify({ objects, opaque: [] }))],
    ["document.json", encoder.encode(JSON.stringify({ version }))],
  ]);
  for (const path of objects) entries.set(path, encoder.encode(`contents of ${path}`));
  return frame(entries).buffer;
}
async function recoveredVersion(handle, generation) {
  const { base } = await handle({ t: "loadCandidate", generation });
  const entries = new Uint8Array(base);
  // document.json is the only JSON entry; the objects' contents hold no braces.
  const text = decoder.decode(entries);
  return JSON.parse(text.slice(text.indexOf("{"), text.indexOf("}") + 1)).version;
}

test("an unreadable journal fails the store instead of being treated as absent", async () => {
  // Session 1 writes version 1 (journal-a, generation 1), then version 2 (journal-b,
  // generation 2), and ends without discarding: version 2 is what recovery must find.
  let handle = await openStore();
  await handle({ t: "checkpoint", entries: checkpointBytes(1) });
  await handle({ t: "checkpoint", entries: checkpointBytes(2) });

  // Session 2 cannot read the newest journal. It must not open the store as if that
  // journal were missing: that offers version 1, collects version 2's pack as garbage
  // and writes the next checkpoint over journal-b.
  fs.failReads.add("journal-b.bin");
  const { handle: second } = await import(`../dist/storage-core.js?session=${++sessions}`);
  await assert.rejects(second({ t: "configure", namespace: "test" }), { name: "NotReadableError" });
  // Later requests on the same store fail the same way; nothing is written.
  await assert.rejects(second({ t: "inspect" }), { name: "NotReadableError" });
  await assert.rejects(second({ t: "checkpoint", entries: checkpointBytes(3) }), {
    name: "NotReadableError",
  });

  // Session 3 reads both journals again and still recovers version 2.
  fs.failReads.clear();
  handle = await openStore();
  const candidates = await handle({ t: "loadCandidates" });
  assert.deepEqual(
    candidates.map((c) => c.generation),
    [2, 1],
  );
  assert.equal(await recoveredVersion(handle, 2), 2);
});

test("a journal that does not exist yet is not an error", async () => {
  const { handle } = await import(`../dist/storage-core.js?session=${++sessions}`);
  await handle({ t: "configure", namespace: "fresh" });
  assert.deepEqual(await handle({ t: "loadCandidates" }), []);
});
