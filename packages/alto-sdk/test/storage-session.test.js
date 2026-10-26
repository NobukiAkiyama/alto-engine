import assert from "node:assert/strict";
import { test } from "node:test";
import { createStorageSession } from "../dist/storage-session.js";

function fakeTimers() {
  const timers = [];
  return {
    timers,
    set(fn, ms) {
      const timer = { fn, ms, cleared: false, fired: false };
      timers.push(timer);
      return timer;
    },
    clear(timer) {
      timer.cleared = true;
    },
    advance(ms) {
      for (const timer of timers) {
        if (!timer.cleared && !timer.fired && timer.ms <= ms) {
          timer.fired = true;
          timer.fn();
        }
      }
    },
  };
}

class FakeWorker {
  constructor() {
    this.messages = [];
    this.terminated = false;
  }
  postMessage(message) {
    this.messages.push(message);
  }
  terminate() {
    this.terminated = true;
  }
  reply(data) {
    this.onmessage({ data });
  }
  fail() {
    this.onerror();
  }
  failMessage() {
    this.onmessageerror();
  }
}

function session(overrides = {}) {
  const worker = overrides.worker ?? new FakeWorker();
  const timers = overrides.timers ?? fakeTimers();
  const inlineCalls = [];
  let inlineLoads = 0;
  const created = createStorageSession({
    createWorker: overrides.createWorker ?? (() => worker),
    loadInline:
      overrides.loadInline ??
      (async () => {
        inlineLoads += 1;
        return {
          handle: async (message) => {
            inlineCalls.push(message);
            return "inline:" + message.t;
          },
        };
      }),
    onInline: overrides.onInline,
    onFailure: overrides.onFailure,
    setTimer: timers.set,
    clearTimer: timers.clear,
    timeoutMs: 3000,
    requestTimeoutMs: 120000,
  });
  return { worker, timers, inlineCalls, inlineLoads: () => inlineLoads, created };
}

test("a worker that answers ping stays running after the startup timer", async () => {
  const { worker, timers, created, inlineLoads } = session();
  worker.reply({ id: 0, ok: true, value: true });
  assert.equal(created.state, "running");
  assert.equal(timers.timers[0].cleared, true);

  const save = created.request("checkpoint", { bytes: new Uint8Array([1, 2, 3]) });
  timers.advance(3000);
  await Promise.resolve();

  assert.equal(created.state, "running");
  assert.equal(worker.terminated, false);
  assert.equal(inlineLoads(), 0);
  const sent = worker.messages.find((message) => message.t === "checkpoint");
  worker.reply({ id: sent.id, ok: true, value: true });
  assert.equal(await save, true);
});

test("startup failure uses the inline journal and still completes an in-flight save", async () => {
  const notices = [];
  const { worker, timers, created, inlineCalls } = session({
    onInline: (why) => notices.push(why),
  });
  const save = created.request("checkpoint", { bytes: new Uint8Array([9]) });
  timers.advance(3000);
  assert.equal(await save, "inline:checkpoint");
  assert.equal(created.state, "inline");
  assert.equal(worker.terminated, true);
  assert.equal(notices[0], "storage worker did not answer");
  assert.equal(inlineCalls[0].t, "checkpoint");
  assert.equal(inlineCalls[0].bytes[0], 9);
});

test("a worker error while starting falls back once", async () => {
  const { worker, timers, created, inlineLoads } = session();
  worker.fail();
  timers.advance(3000);
  assert.equal(await created.request("inspect"), "inline:inspect");
  assert.equal(inlineLoads(), 1);
  assert.equal(created.state, "inline");
});

test("a worker that cannot be constructed falls back and serves the save", async () => {
  const { created } = session({
    createWorker: () => {
      throw new Error("Worker is not defined");
    },
  });
  assert.equal(await created.request("checkpoint", { bytes: new Uint8Array([4]) }), "inline:checkpoint");
  assert.equal(created.state, "inline");
});

test("a worker error after startup rejects an in-flight save without replaying it", async () => {
  const failures = [];
  const { worker, created, inlineLoads, timers } = session({
    onFailure: (error) => failures.push(error),
  });
  worker.reply({ id: 0, ok: true, value: true });
  const save = created.request("append", { bytes: new Uint8Array([7]) });
  worker.fail();
  await assert.rejects(save, /STORAGE: storage worker failed/);
  assert.equal(created.state, "failed");
  assert.equal(worker.terminated, true);
  assert.equal(inlineLoads(), 0);
  assert.equal(failures.length, 1);
  assert.equal(timers.timers[1].cleared, true);
  const sent = worker.messages.find((message) => message.t === "append");
  worker.reply({ id: sent.id, ok: true, value: true });
  await assert.rejects(created.request("checkpoint"), /STORAGE: storage worker failed/);
});

test("an unreadable reply and a silent worker each reject pending requests", async () => {
  for (const breakWorker of [
    ({ worker }) => worker.failMessage(),
    ({ timers }) => timers.advance(120000),
  ]) {
    const fixture = session();
    fixture.worker.reply({ id: 0, ok: true, value: true });
    const first = fixture.created.request("append", { bytes: new Uint8Array([1]) });
    const second = fixture.created.request("inspect");
    breakWorker(fixture);
    await assert.rejects(first, /STORAGE: storage worker/);
    await assert.rejects(second, /STORAGE: storage worker/);
    assert.equal(fixture.created.state, "failed");
    assert.equal(fixture.worker.terminated, true);
  }
});

test("a failed inline fallback rejects requests instead of waiting forever", async () => {
  const { worker, created } = session({
    loadInline: async () => {
      throw new Error("module unavailable");
    },
  });
  worker.fail();
  await assert.rejects(created.request("inspect"), /STORAGE: inline journal unavailable/);
  assert.equal(created.state, "failed");
});
