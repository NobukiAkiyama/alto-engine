import assert from "node:assert/strict";
import { test } from "node:test";
import { listRecoveryNamespaces } from "../dist/recovery-store.js";

test("recovery listing tolerates files retired during enumeration", async () => {
  const folder = {
    kind: "directory",
    async *entries() {
      yield ["old-pack", { kind: "file", async getFile() { throw new DOMException("removed", "NotFoundError"); } }];
      yield ["journal", { kind: "file", async getFile() { return { size: 20, lastModified: 42 }; } }];
    },
    async getFileHandle() { throw new DOMException("missing", "NotFoundError"); },
  };
  const original = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  const directory = { async *entries() { yield ["ns-666f6f", folder]; } };
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: { storage: { async getDirectory() {
      return { async getDirectoryHandle() { return directory; } };
    } } },
  });
  try {
    assert.deepEqual(await listRecoveryNamespaces(), [{ namespace: "foo", bytes: 20, modified: 42 }]);
  } finally {
    if (original) Object.defineProperty(globalThis, "navigator", original);
    else delete globalThis.navigator;
  }
});
