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

/** A checkpoint of `version` naming `listed`, carrying the bytes of `carried`. */
function checkpointOf(version, listed, carried = listed) {
  const objects = listed.map(object);
  const entries = new Map([
    ["checkpoint.json", encoder.encode(JSON.stringify({ objects, opaque: [] }))],
    ["document.json", encoder.encode(JSON.stringify({ version }))],
  ]);
  for (const path of carried.map(object)) entries.set(path, encoder.encode(`contents of ${path}`));
  return frame(entries).buffer;
}
/** Pack files in a namespace, found by walking the fake OPFS. */
function packNames(namespace) {
  const folder = "ns-" + Array.from(encoder.encode(namespace), (b) => b.toString(16).padStart(2, "0")).join("");
  const find = (dir) => {
    for (const [name, child] of dir.children ?? []) {
      if (name === folder) return child.children.get("packs");
      const found = child.kind === "directory" && find(child);
      if (found) return found;
    }
  };
  return [...(find(root)?.children.keys() ?? [])].sort();
}

test("packs neither journal names are collected mid-session, and their objects are written again", async () => {
  const handle = await (async () => {
    const { handle } = await import(`../dist/storage-core.js?session=${++sessions}`);
    await handle({ t: "configure", namespace: "collect" });
    return handle;
  })();
  await handle({ t: "checkpoint", entries: checkpointOf(1, [1, 2]) });
  await handle({ t: "checkpoint", entries: checkpointOf(2, [3]) });
  // Both journals still name a pack each: nothing is dead.
  assert.deepEqual((await handle({ t: "collectPlan" })).packs, []);
  await handle({ t: "checkpoint", entries: checkpointOf(3, [4]) });
  const plan = await handle({ t: "collectPlan" });
  assert.deepEqual(plan.packs, ["1.pack"]);
  assert.deepEqual(plan.objects.sort(), [object(1), object(2)]);
  assert.deepEqual(packNames("collect"), ["1.pack", "2.pack", "3.pack"]);
  const result = await handle({ t: "collect", packs: plan.packs });
  assert.equal(result.packs, 1);
  assert.deepEqual(packNames("collect"), ["2.pack", "3.pack"]);
  assert.ok(result.bytes > 0);
  assert.equal((await handle({ t: "collectPlan" })).packs.length, 0);

  // Object 1 left the store: a checkpoint that counts on it being stored is refused,
  // and one that carries it again is written.
  await assert.rejects(handle({ t: "checkpoint", entries: checkpointOf(4, [1, 4], []) }), /lacks/);
  await handle({ t: "checkpoint", entries: checkpointOf(4, [1, 4], [1]) });

  // A pack a journal names is never deleted, even when a plan named it.
  const plan2 = await handle({ t: "collectPlan" });
  assert.deepEqual((await handle({ t: "collect", packs: ["3.pack", "4.pack", ...plan2.packs] })).packs, plan2.packs.length);

  // A new session recovers the newest checkpoint and reads every object it names.
  const { handle: next } = await import(`../dist/storage-core.js?session=${++sessions}`);
  await next({ t: "configure", namespace: "collect" });
  const candidates = await next({ t: "loadCandidates" });
  assert.equal(candidates[0].generation, 4);
  assert.equal(await recoveredVersion(next, candidates[0].generation), 4);
  // The older journal is still whole too: its packs were kept.
  assert.equal(await recoveredVersion(next, candidates[1].generation), 3);
});

test("a session that starts on two journals collects only what neither names", async () => {
  // Session 1 leaves two journals and three packs; the first pack is already dead.
  let handle = await (async () => {
    const { handle } = await import(`../dist/storage-core.js?session=${++sessions}`);
    await handle({ t: "configure", namespace: "resume" });
    return handle;
  })();
  await handle({ t: "checkpoint", entries: checkpointOf(1, [1, 2]) });
  await handle({ t: "checkpoint", entries: checkpointOf(2, [3]) });
  await handle({ t: "checkpoint", entries: checkpointOf(3, [5]) });
  // Session 2 recovers, as the engine Worker does, then writes and collects.
  const { handle: next } = await import(`../dist/storage-core.js?session=${++sessions}`);
  await next({ t: "configure", namespace: "resume" });
  const candidates = await next({ t: "loadCandidates" });
  assert.deepEqual(candidates.map((c) => c.generation), [3, 2]);
  assert.equal(await recoveredVersion(next, 3), 3);
  await next({ t: "activate", generation: 3 });
  // Before the session's first write nothing is collected mid-session; the startup
  // collection takes the dead pack with that write.
  assert.deepEqual((await next({ t: "collectPlan" })).packs, []);
  await next({ t: "checkpoint", entries: checkpointOf(4, [5, 6], [6]) });
  assert.ok(!packNames("resume").includes("1.pack"), "the startup collection kept a dead pack");
  const plan = await next({ t: "collectPlan" });
  // Journals 4 and 3 name packs 3 and 4 (or 4 alone after a merge); pack 2 is dead.
  assert.deepEqual(plan.packs, ["2.pack"]);
  assert.deepEqual(plan.objects, [object(3)]);
  assert.equal((await next({ t: "collect", packs: plan.packs })).packs, 1);
  // Both journals still recover in a third session.
  const { handle: third } = await import(`../dist/storage-core.js?session=${++sessions}`);
  await third({ t: "configure", namespace: "resume" });
  const after = await third({ t: "loadCandidates" });
  assert.deepEqual(after.map((c) => c.generation), [4, 3]);
  assert.equal(await recoveredVersion(third, 4), 4);
  assert.equal(await recoveredVersion(third, 3), 3);
});
