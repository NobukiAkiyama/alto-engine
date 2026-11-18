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

// Import a fresh worker module for each simulated restart.
let session = 0;
async function open() {
  const { handle, namespaceFolder } = await import(`../dist/storage-core.js?mutation=${++session}`);
  await handle({ t: "configure", namespace: "mutation" });
  return { handle, namespaceFolder };
}
const bytes = (s) => new TextEncoder().encode(s);

async function fixture() {
  const { handle, namespaceFolder } = await open();
  await handle({ t: "checkpointLegacy", bytes: bytes("base").buffer });
  for (const value of ["first", "second", "third"])
    await handle({ t: "append", bytes: bytes(value).buffer });
  const dir = root.children.get("alto-engine").children.get(namespaceFolder("mutation"));
  const file = dir.children.get("journal-a.bin");
  const original = file.data.slice();
  const offsets = [];
  let at = 12;
  while (at < original.length) {
    offsets.push(at);
    at += 8 + new DataView(original.buffer, original.byteOffset + at, 4).getUint32(0, true);
  }
  assert.equal(at, original.length);
  assert.equal(offsets.length, 4);
  return { file, original, offsets };
}

async function check(file, input, expected, truncated) {
  file.data = input;
  const { handle } = await open();
  const candidates = await handle({ t: "loadCandidates" });
  if (expected === null) {
    assert.deepEqual(candidates, []);
    return;
  }
  assert.equal(candidates.length, 1);
  assert.equal(candidates[0].truncated, truncated);
  const result = await handle({ t: "loadCandidate", generation: candidates[0].generation });
  assert.deepEqual(result.base, bytes("base"));
  assert.deepEqual(result.records.map((r) => new TextDecoder().decode(r)), expected);
}

test("journal mutations retain exactly the valid prefix", async () => {
  const { file, original, offsets } = await fixture();
  const values = ["first", "second", "third"];
  await check(file, original.slice(), values, false);
  // A broken magic or an incomplete header cannot supply a checkpoint.
  for (let n = 0; n < 12; n++) await check(file, original.slice(0, n), null, false);
  for (let p = 0; p < 8; p++) {
    const damaged = original.slice();
    damaged[p] ^= 1;
    await check(file, damaged, null, false);
  }
  // Every truncation, including exactly on a record boundary.
  for (let end = 12; end <= original.length; end++) {
    const complete = offsets.filter((_, i) => (offsets[i + 1] ?? original.length) <= end).length;
    const expected = complete === 0 ? null : values.slice(0, complete - 1);
    const boundary = end === 12 || offsets.includes(end) || end === original.length;
    await check(file, original.slice(0, end), expected, !boundary);
  }
  // Flip each bit in every header and payload, leaving later full records in place.
  for (let i = 0; i < offsets.length; i++) {
    const end = offsets[i + 1] ?? original.length;
    for (let p = offsets[i]; p < end; p++) {
      for (let bit = 0; bit < 8; bit++) {
        const damaged = original.slice();
        damaged[p] ^= 1 << bit;
        await check(file, damaged, i === 0 ? null : values.slice(0, i - 1), true);
      }
    }
  }
  // Explicit zero and huge lengths must stop before allocating or reaching later records.
  for (const length of [0, 0xffffffff]) {
    const damaged = original.slice();
    new DataView(damaged.buffer).setUint32(offsets[2], length, true);
    await check(file, damaged, ["first"], true);
  }
  // A checksum-valid but structurally invalid record must also stop the scan.
  const malformed = original.slice();
  const at = offsets[2];
  const length = new DataView(malformed.buffer).getUint32(at, true);
  malformed[at + 8] = 99;
  let crc = 0xffffffff;
  for (const byte of malformed.subarray(at + 8, at + 8 + length)) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = crc & 1 ? 0xedb88320 ^ (crc >>> 1) : crc >>> 1;
  }
  new DataView(malformed.buffer).setUint32(at + 4, (crc ^ 0xffffffff) >>> 0, true);
  await check(file, malformed, ["first"], true);
});
