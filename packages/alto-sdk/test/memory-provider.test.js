import { test } from "node:test";
import assert from "node:assert/strict";
import { MemoryObjectProvider, sha256Hex } from "../dist/storage.js";
test("exclusive storage serializes writes and fences escaped handles", async () => {
  const provider = new MemoryObjectProvider(); const events = []; let release, entered, escaped;
  const gate = new Promise((r) => { release = r; }); const inside = new Promise((r) => { entered = r; });
  const first = provider.exclusive(async (view) => { escaped = view; events.push("start"); entered(); await gate; await view.writeRef("manifest.json", new Uint8Array([1])); events.push("finish"); });
  await inside; const second = provider.writeRef("manifest.json", new Uint8Array([2])).then(() => events.push("second"));
  await Promise.resolve(); assert.deepEqual(events, ["start"]); release(); await Promise.all([first, second]);
  assert.deepEqual(events, ["start", "finish", "second"]); await assert.rejects(escaped.writeRef("manifest.json", new Uint8Array([3])), /lease expired/);
  assert.deepEqual(await provider.readRef("manifest.json"), new Uint8Array([2]));
});
test("manifest compare-and-replace admits only one concurrent writer", async () => {
  const provider = new MemoryObjectProvider(); await provider.writeRef("manifest.json", new Uint8Array([0])); const baseline = await sha256Hex(new Uint8Array([0]));
  const result = await Promise.allSettled([provider.writeRef("manifest.json", new Uint8Array([1]), baseline), provider.writeRef("manifest.json", new Uint8Array([2]), baseline)]);
  assert.equal(result.filter((r) => r.status === "fulfilled").length, 1); assert.equal(result.find((r) => r.status === "rejected").reason.code, "CONFLICT");
  const bytes = await provider.readRef("manifest.json"); bytes[0] = 9; assert.deepEqual(await provider.readRef("manifest.json"), new Uint8Array([1]));
});
