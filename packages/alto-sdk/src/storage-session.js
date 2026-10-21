// Journal transport for the engine worker. The nested Storage Worker is used only after
// it answers ping. Until then the session is starting; if it never starts, the same
// journal runs inline. A running worker is not replaced when the startup timer elapses.
export function createStorageSession({
  createWorker,
  loadInline,
  onInline = () => {},
  onFailure = () => {},
  setTimer = (fn, ms) => setTimeout(fn, ms),
  clearTimer = (id) => clearTimeout(id),
  timeoutMs = 3000,
  requestTimeoutMs = 120000,
}) {
  let mode = "starting";
  let closing = false;
  let failure = null;
  let worker = null;
  let inline = null;
  let timer = null;
  let sequence = 1;
  const pending = new Map();
  let resolveReady;
  const ready = new Promise((resolve) => {
    resolveReady = resolve;
  });

  function stopTimer() {
    if (timer != null) clearTimer(timer);
    timer = null;
  }

  async function callInline(message) {
    try {
      return await inline.handle(message);
    } catch (error) {
      throw Error("STORAGE: " + error);
    }
  }

  function failRunning(why) {
    if (mode !== "running") return;
    mode = "failed";
    failure = Error("STORAGE: " + why);
    const dead = worker;
    worker = null;
    dead?.terminate();
    for (const entry of pending.values()) {
      clearTimer(entry.timer);
      entry.reject(failure);
    }
    pending.clear();
    try { onFailure(failure); } catch {}
    // A write may have reached OPFS before its reply was lost. Replaying it
    // inline could append the same record twice, so this session fails closed.
  }

  async function failStart(why) {
    if (mode !== "starting" || closing) return;
    closing = true;
    stopTimer();
    const dead = worker;
    worker = null;
    dead?.terminate();
    try {
      inline = await loadInline();
    } catch (error) {
      failure = Error("STORAGE: inline journal unavailable: " + error);
      mode = "failed";
      resolveReady();
      try { onFailure(failure); } catch {}
      return;
    }
    const queued = [...pending.values()];
    pending.clear();
    mode = "inline";
    onInline(why);
    resolveReady();
    for (const entry of queued) {
      try {
        entry.resolve(await callInline(entry.message));
      } catch (error) {
        entry.reject(error);
      }
    }
  }

  function onMessage(data) {
    if (data.id === 0) {
      if (mode !== "starting" || closing) return;
      mode = "running";
      stopTimer();
      resolveReady();
      return;
    }
    const entry = pending.get(data.id);
    if (!entry || mode !== "running") return;
    pending.delete(data.id);
    clearTimer(entry.timer);
    data.ok
      ? entry.resolve(data.value)
      : entry.reject(Error("STORAGE: " + data.error));
  }

  try {
    worker = createWorker();
    worker.onmessage = (event) => onMessage(event.data);
    worker.onerror = () => {
      if (mode === "running") failRunning("storage worker failed");
      else void failStart("storage worker failed to start");
    };
    worker.onmessageerror = () => {
      if (mode === "running") failRunning("storage worker reply could not be read");
      else void failStart("storage worker reply could not be read");
    };
    worker.postMessage({ t: "ping", id: 0 });
    timer = setTimer(() => {
      void failStart("storage worker did not answer");
    }, timeoutMs);
  } catch (error) {
    void failStart(String(error));
  }

  return {
    get state() {
      return mode;
    },
    async request(t, payload = {}, transfer = []) {
      const message = { t, ...payload };
      if (mode !== "running") await ready;
      if (mode === "failed") throw failure;
      if (mode === "inline") return callInline(message);
      const id = sequence++;
      const sent = { ...message, id };
      return new Promise((resolve, reject) => {
        const timer = setTimer(
          () => failRunning("storage worker did not answer request " + id),
          requestTimeoutMs,
        );
        pending.set(id, { resolve, reject, message: sent, timer });
        try {
          worker.postMessage(sent, transfer);
        } catch (error) {
          pending.delete(id);
          clearTimer(timer);
          reject(Error("STORAGE: " + error));
        }
      });
    },
  };
}
