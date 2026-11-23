import { test } from "node:test";
import assert from "node:assert/strict";
import { FileSystemDirectoryProvider } from "../dist/storage.js";

// Safari 27 (WebKit) writes a view's whole underlying buffer from its start when given
// a Uint8Array or DataView that covers only part of it. This handle does the same.
function safariLikeDirectory() {
  const files = new Map();
  const notFound = () => Object.assign(new Error("not found"), { name: "NotFoundError" });
  const dir = (prefix) => ({
    async getDirectoryHandle(name) { return dir(prefix + name + "/"); },
    async getFileHandle(name, { create = false } = {}) {
      const path = prefix + name;
      if (!files.has(path) && !create) throw notFound();
      return {
        async getFile() { if (!files.has(path)) throw notFound(); return new Blob([files.get(path)]); },
        async createWritable() {
          const parts = [];
          return {
            async write(data) {
              if (ArrayBuffer.isView(data)) parts.push(new Uint8Array(data.buffer).slice());
              else parts.push(new Uint8Array(await new Blob([data]).arrayBuffer()));
            },
            async close() { files.set(path, new Uint8Array(await new Blob(parts).arrayBuffer())); },
            async abort() {},
          };
        },
      };
    },
  });
  return dir("");
}

test("a part of a larger buffer is written as itself, not as the whole buffer", async () => {
  const provider = new FileSystemDirectoryProvider(safariLikeDirectory());
  const framed = new Uint8Array(64).map((_, i) => i);
  const manifest = framed.subarray(10, 20);
  await provider.writeRef("manifest.json", manifest);
  assert.deepEqual(await provider.readRef("manifest.json"), manifest.slice());
  await provider.putObjects([{ path: "objects/ab/cdef", data: framed.subarray(30, 34) }]);
  assert.deepEqual(await provider.getObject("objects/ab/cdef"), framed.slice(30, 34));
});
