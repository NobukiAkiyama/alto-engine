// MessageChannel yields to Worker messages without nested timer clamping.
const yieldTask = () => new Promise((resolve) => {
  const channel = new MessageChannel();
  channel.port1.onmessage = () => {
    channel.port1.close(); channel.port2.close(); resolve();
  };
  channel.port2.postMessage(null);
});
import init, { Runtime } from "./pkg/alto_wasm.js";
import { createStorageSession } from "./storage-session.js";
self.addEventListener("error", (e) => {
  postMessage({
    t: "diagnostic",
    value: {
      message: e.message,
      file: e.filename,
      line: e.lineno,
      error: String(e.error),
    },
  });
});
let journalBacklog = [];
let diagnosticProfiles = null;
let injectedStorageFault = null;
const readableAfterFailure = new Set([
  "emergencyExport", "save", "view", "waitFrame", "readTile", "readTileLevel",
  "selectionOutline", "textInfo", "textHitTest", "textCaret",
  "textSelection", "textNavigate", "versionLog", "versionDiff",
  "vectorStats", "vectorPathSvg",
]);
let storageFailure = null;
function freezeStorage(error) {
  if (storageFailure) return;
  storageFailure = structured(error);
  postMessage({ t: "storageFailure", value: storageFailure });
}
let runtime,
  canvas,
  budget = 512,
  queue = [],
  draining = false,
  wantTick = false,
  pendingViewport = null,
  pendingResize = null,
  viewVersion = 0,
  frameSettled = false,
  prefetchTimer = null,
  prefetchDisabled = false,
  prefetchSpentMs = 0,
  lastMessageAt = performance.now();
function schedulePrefetch() {
  if (prefetchTimer !== null || !frameSettled || prefetchDisabled || !runtime || storageFailure)
    return;
  const quietFor = performance.now() - lastMessageAt;
  const delay = prefetchSpentMs >= 4 ? Math.max(0, 100 - quietFor) : 0;
  prefetchTimer = setTimeout(() => {
    prefetchTimer = null;
    if (draining || queue.length || wantTick || pendingViewport || pendingResize || !frameSettled)
      return;
    if (prefetchSpentMs >= 4 && performance.now() - lastMessageAt < 100) {
      schedulePrefetch();
      return;
    }
    try {
      const started = performance.now();
      const status = runtime.prefetchStep();
      prefetchSpentMs += performance.now() - started;
      // Each call returns to the event loop so an incoming edit or visible frame wins.
      if (status === "progress" || status === "ready") schedulePrefetch();
    } catch (error) {
      if (String(error).includes("GPU_LOST")) {
        wantTick = true;
        void drain();
      } else {
        prefetchDisabled = true;
        postMessage({ t: "diagnostic", value: { message: "prefetch disabled: " + error } });
      }
    }
  }, delay);
}
// The journal normally runs in a nested Storage Worker. Some embedded browsers cannot
// start workers from a worker; then the same journal code runs on this thread.
const journal = createStorageSession({
  createWorker: () =>
    new Worker(new URL("./storage-worker.js", import.meta.url), {
      type: "module",
    }),
  loadInline: () => import("./storage-core.js"),
  onInline(why) {
    postMessage({ t: "diagnostic-storage", value: "inline journal: " + why });
  },
  onFailure(error) {
    freezeStorage(error);
  },
});
async function io(t, payload = {}, transfer = []) {
  try {
    return await journal.request(t, payload, transfer);
  } catch (error) {
    freezeStorage(error);
    throw error;
  }
}
function structured(error) {
  const message = error instanceof Error ? error.message : String(error);
  const code =
    [
      "INVALID_ARGUMENT",
      "NOT_FOUND",
      "CONFLICT",
      "GPU_LOST",
      "GPU_UNSUPPORTED",
      "CORRUPT_DATA",
      "STORAGE",
      "INTERNAL",
    ].find(
      (c) => message === c || message.startsWith(`${c}:`) || message.startsWith(`${c} `),
    ) ||
    (/unreachable|borrow|RuntimeError/.test(message)
      ? "INTERNAL"
      : "INVALID_ARGUMENT");
  return { code, message };
}
async function flushJournal() {
  let b;
  while ((b = runtime.takeJournal()).length) journalBacklog.push(b);
  while (journalBacklog.length) {
    if (injectedStorageFault === "beforeAppend") {
      injectedStorageFault = null;
      const error = Error("STORAGE: injected failure before append");
      freezeStorage(error);
      throw error;
    }
    const a = journalBacklog[0].slice();
    await io("append", { bytes: a.buffer }, [a.buffer]);
    if (injectedStorageFault === "afterAppend") {
      injectedStorageFault = null;
      const error = Error("STORAGE: injected lost reply after append");
      freezeStorage(error);
      throw error;
    }
    journalBacklog.shift();
  }
}

async function checkpoint() {
  const b = runtime.savePackage();
  await persistCheckpoint(b);
  journalBacklog = [];
}
async function persistCheckpoint(bytes) {
  const opaque = JSON.parse(runtime.recoveryOpaque());
  await io("checkpoint", { bytes: bytes.buffer, opaque }, [bytes.buffer]);
}
let savedToken = 0;
const saves = new Map();
async function execute(m) {
  // The in-memory runtime may be ahead of the last confirmed journal write.
  // Keep it available for an emergency export, but accept no more edits.
  if (storageFailure && !readableAfterFailure.has(m.t))
    throw Error("STORAGE: 保存状態を確認できません。現在の内容を退避してから復旧してください");
  switch (m.t) {
    case "init":
      if (m.protocol !== 4) throw Error("CONFLICT: protocol mismatch");
      canvas = m.canvas;
      budget = m.budget;
      await init();
      runtime = await Runtime.create(canvas, budget);
      if (runtime.protocolVersion?.() !== 4)
        throw Error("CONFLICT: SDK/WASM version mismatch");
      return { protocol: 4, recovery: await io("inspect") };
    case "createDocument": {
      const state = JSON.parse(runtime.createDocument(m.width, m.height));
      await checkpoint();
      return state;
    }
    case "transact": {
      const s = JSON.parse(
        runtime.transact(
          JSON.stringify({
            intent: m.intent,
            ops: m.ops,
            baseRevision: m.baseRevision,
          }),
        ),
      );
      await flushJournal();
      postMessage({ t: "patch", value: s });
      return s;
    }
    case "undo":
    case "redo": {
      const s = JSON.parse(runtime[m.t]());
      await flushJournal();
      postMessage({ t: "patch", value: s });
      return s;
    }
    case "beginBrush":
      runtime.beginBrush(
        m.layer,
        JSON.stringify(m.config),
        new Float32Array(m.color),
        m.erase,
        !!m.mask,
      );
      return true;
    case "input":
      runtime.brushInput(m.samples);
      return true;
    case "endBrush": {
      const s = JSON.parse(await runtime.endBrush());
      await flushJournal();
      postMessage({ t: "patch", value: s });
      return s;
    }
    case "cancelBrush":
      runtime.cancelBrush();
      return true;
    case "select":
    case "setMask":
    case "setEffects": {
      const result =
        m.t === "select"
          ? runtime.select(JSON.stringify(m.spec))
          : m.t === "setMask"
            ? runtime.setMask(m.layer, m.source)
            : runtime.setEffects(
                m.layer,
                JSON.stringify(m.effects),
                !!m.useSelection,
              );
      const state = JSON.parse(result);
      await flushJournal();
      postMessage({ t: "patch", value: state });
      return state;
    }
    case "selectionOutline":
      return runtime.selectionOutline();
    case "import":
      return JSON.parse(
        runtime.importRgba(
          m.width,
          m.height,
          new Uint8Array(m.pixels),
          new Uint8Array(m.encoded),
          m.mime,
        ),
      );
    case "emergencyExport":
      return runtime.savePackage();
    case "save": {
      const state = JSON.parse(runtime.view());
      const bytes = runtime.savePackage();
      const token = ++savedToken;
      saves.set(token, {
        hash: state.stateHash,
        revision: state.document.revision,
        bytes: bytes.slice(),
      });
      return { bytes, token };
    }
    case "folderEntries": {
      const state = JSON.parse(runtime.view());
      const bytes = runtime.folderEntries();
      const token = ++savedToken;
      saves.set(token, {
        hash: state.stateHash,
        revision: state.document.revision,
        bytes: runtime.checkpointBytes(),
      });
      return { bytes, token };
    }
    case "folderPlan":
      return JSON.parse(runtime.folderPlan(new Uint8Array(m.bytes)));
    case "openFolder": {
      const s = JSON.parse(runtime.openFolder(new Uint8Array(m.bytes)));
      const base = runtime.checkpointBytes();
      await persistCheckpoint(base);
      journalBacklog = [];
      return s;
    }
    case "importFont":
      return JSON.parse(runtime.importFont(new Uint8Array(m.bytes), m.faceIndex));
    case "releaseImport":
      return runtime.releaseImport(m.hash);
    case "pruneVersions": {
      const state = JSON.parse(runtime.pruneVersions());
      await flushJournal();
      await checkpoint();
      postMessage({ t: "patch", value: state });
      return state;
    }
    case "textInfo":
      return JSON.parse(runtime.textInfo(m.node));
    case "textHitTest":
      return runtime.textHitTest(m.node, m.x, m.y);
    case "textCaret":
      return JSON.parse(runtime.textCaret(m.node, m.offset));
    case "textSelection":
      return JSON.parse(runtime.textSelection(m.node, m.start, m.end));
    case "textNavigate":
      return runtime.textNavigate(m.node, m.offset, m.motion);
    case "commit": {
      const commit = JSON.parse(
        runtime.commit(m.message, m.time, m.author, !!m.allowEmpty),
      );
      const state = JSON.parse(runtime.view());
      // Commits are not journaled operations: checkpoint so a crash keeps them.
      await flushJournal();
      await checkpoint();
      postMessage({ t: "patch", value: state });
      return { commit, state };
    }
    case "versionLog":
      return JSON.parse(runtime.versionLog());
    case "versionDiff":
      return JSON.parse(runtime.versionDiff(m.from, m.to));
    case "restoreVersion": {
      const state = JSON.parse(runtime.restoreVersion(m.hash));
      await flushJournal();
      postMessage({ t: "patch", value: state });
      return state;
    }
    case "saved": {
      const saved = saves.get(m.token);
      if (!saved) throw Error("INVALID_ARGUMENT: save token expired");
      const state = JSON.parse(runtime.markSaved(saved.hash));
      if (state.document.revision === saved.revision) {
        await persistCheckpoint(saved.bytes);
        journalBacklog = [];
      }
      saves.delete(m.token);
      return state;
    }
    case "open": {
      const s = JSON.parse(runtime.openPackage(new Uint8Array(m.bytes)));
      await persistCheckpoint(new Uint8Array(m.bytes));
      journalBacklog = [];
      return s;
    }
    case "recover": {
      const candidates = await io("loadCandidates");
      if (!candidates.length) {
        const state = await io("inspect");
        throw Error(state.latestGeneration
          ? "CORRUPT_DATA: no usable recovery generation"
          : "NOT_FOUND: recovery journal");
      }
      let recovery = null;
      let lastError;
      for (const candidate of candidates) {
        try {
          if (candidate.metadata)
            runtime.openRecoveryPackage(candidate.base, JSON.stringify(candidate.metadata.opaque));
          else runtime.openPackage(candidate.base);
          for (const r of candidate.records) runtime.replayRecord(r);
          recovery = candidate;
          break;
        } catch (error) {
          lastError = error;
        }
      }
      if (!recovery) throw Error("CORRUPT_DATA: no usable recovery generation: " + lastError);
      await io("activate", { generation: recovery.generation });
      const s = JSON.parse(runtime.view());
      if (recovery.generation !== recovery.latestGeneration)
        s.warnings.push("新しい復旧記録が破損していたため、前の世代を開きました");
      if (recovery.truncated)
        s.warnings.push("最後の不完全な記録を除いて復旧しました");
      return s;
    }
    case "validateFrame":
      await runtime.validateFrame();
      return true;
    case "cpuReference":
      return runtime.cpuBrushReference(
        JSON.stringify(m.config),
        new Float32Array(m.color),
        new Float32Array(m.samples),
      );
    case "corruptJournalTail":
      return io("corruptTail");
    case "corruptCheckpoint":
      return io("corruptCheckpoint");
    case "corruptCheckpointCrc":
      return io("corruptCheckpointCrc");
    case "corruptBothCheckpoints":
      return io("corruptBothCheckpoints");
    case "legacyCheckpoint": {
      const bytes = runtime.savePackage();
      await io("checkpointLegacy", { bytes: bytes.buffer }, [bytes.buffer]);
      journalBacklog = [];
      return true;
    }
    case "injectStorageFault":
      if (m.position !== "beforeAppend" && m.position !== "afterAppend")
        throw Error("INVALID_ARGUMENT: storage fault position");
      injectedStorageFault = m.position;
      return true;
    case "parseVectorPath": return JSON.parse(runtime.parseVectorPath(m.svg));
    case "vectorPathSvg": return runtime.vectorPathSvg(JSON.stringify(m.data));
    case "vectorStats": return JSON.parse(runtime.vectorStats());
    case "setGlyphAtlas": runtime.setGlyphAtlas(!!m.enabled); return true;
    case "readTileLevel": return await runtime.readTileLevel(m.level,m.x,m.y);
    case "readTile":
      return await runtime.readTile(m.x, m.y);
    case "destroyGpu":
      runtime.destroyGpu();
      return true;
    case "setVisibleTileJobs":
      runtime.setVisibleTileJobs(!!m.enabled);
      return true;
    case "resetRenderCache":
      prefetchDisabled = true;
      runtime.resetRenderCache(!!m.cpu);
      diagnosticProfiles = [];
      return true;
    case "enablePrefetch":
      prefetchDisabled = false;
      return true;
    case "setProfiling":
      runtime.setProfiling(!!m.enabled);
      return true;
    case "waitFrame": {
      let frame;
      const profiles = diagnosticProfiles || [];
      do {
        frame = JSON.parse(runtime.tick(8));
        if (frame.profile) profiles.push(frame.profile);
        if (frame.pending) await yieldTask();
      } while (frame.pending);
      await runtime.gpuIdle();
      diagnosticProfiles = null;
      frameSettled = true;
      prefetchSpentMs = 0;
      if (profiles.length) {
        frame.profile = Object.fromEntries(Object.keys(profiles[0]).map((key) =>
          [key, key === "maxStepMs" ? profiles.reduce((max, profile) => Math.max(max, profile[key]), 0) : profiles.reduce((sum, profile) => sum + profile[key], 0)]));
      }
      return { ...frame, viewVersion };
    }
    case "view":
      return JSON.parse(runtime.view());
    default:
      throw Error("INVALID_ARGUMENT: unknown request " + m.t);
  }
}
async function drain() {
  if (draining) return;
  draining = true;
  try {
    while (queue.length || wantTick || pendingViewport || pendingResize) {
      if (runtime && pendingResize) {
        runtime.resize(pendingResize.width, pendingResize.height);
        pendingResize = null;
      }
      if (runtime && pendingViewport) {
        const v = pendingViewport;
        runtime.viewport(v.center[0], v.center[1], v.zoom, v.rotation);
        viewVersion = v.version;
        pendingViewport = null;
      }
      if (queue.length) {
        const m = queue.shift();
        try {
          const value = await execute(m);
          if (m.id)
            postMessage(
              { t: "reply", id: m.id, ok: true, value },
              value?.bytes instanceof Uint8Array
                ? [value.bytes.buffer]
                : value instanceof Uint8Array
                  ? [value.buffer]
                  : [],
            );
        } catch (error) {
          const e = structured(error);
          if (m.id) postMessage({ t: "reply", id: m.id, ok: false, error: e });
          else postMessage({ t: "error", value: e });
        }
      } else {
        wantTick = false;
        if (runtime)
          try {
            const value = { ...JSON.parse(runtime.tick(8)), viewVersion };
            if (value.profile && diagnosticProfiles) diagnosticProfiles.push(value.profile);
            frameSettled = value.pending === 0;
            if (value.pending) wantTick = true;
            if (frameSettled) prefetchSpentMs = 0;
            postMessage({ t: "frame", value });
            if (value.pending) await yieldTask();
          } catch (error) {
            if (String(error).includes("GPU_LOST")) {
              postMessage({ t: "gpuLost" });
              try {
                await runtime.recoverGpu();
                postMessage({ t: "gpuRestored" });
              } catch (e) {
                postMessage({ t: "error", value: structured(e) });
              }
            } else postMessage({ t: "error", value: structured(error) });
          }
      }
    }
  } finally {
    draining = false;
    schedulePrefetch();
  }
}

self.onmessage = ({ data: m }) => {
  lastMessageAt = performance.now();
  frameSettled = false;
  if (prefetchTimer !== null) {
    clearTimeout(prefetchTimer);
    prefetchTimer = null;
  }
  if (m.t === "tick") {
    wantTick = true;
  } else if (m.t === "viewport") {
    pendingViewport = m;
    wantTick = true;
  } else if (m.t === "resize") {
    pendingResize = m;
    wantTick = true;
  } else queue.push(m);
  void drain();
};
