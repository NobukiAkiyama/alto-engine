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
let runtime,
  canvas,
  budget = 512,
  queue = [],
  draining = false,
  wantTick = false,
  pendingViewport = null,
  pendingResize = null,
  viewVersion = 0;
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
});
function io(t, payload = {}, transfer = []) {
  return journal.request(t, payload, transfer);
}
function structured(error) {
  const message = String(error);
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
    const a = journalBacklog[0].slice();
    await io("append", { bytes: a.buffer }, [a.buffer]);
    journalBacklog.shift();
  }
}

async function checkpoint() {
  const b = runtime.savePackage();
  await io("checkpoint", { bytes: b.buffer }, [b.buffer]);
  journalBacklog = [];
}
let savedToken = 0;
const saves = new Map();
async function execute(m) {
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
      const s = JSON.parse(runtime.transact(JSON.stringify(m)));
      postMessage({ t: "patch", value: s });
      await flushJournal();
      return s;
    }
    case "undo":
    case "redo": {
      const s = JSON.parse(runtime[m.t]());
      postMessage({ t: "patch", value: s });
      await flushJournal();
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
      postMessage({ t: "patch", value: s });
      await flushJournal();
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
      postMessage({ t: "patch", value: state });
      await flushJournal();
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
      await io("checkpoint", { bytes: base.buffer }, [base.buffer]);
      journalBacklog = [];
      return s;
    }
    case "importFont":
      return JSON.parse(runtime.importFont(new Uint8Array(m.bytes), m.faceIndex));
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
      postMessage({ t: "patch", value: state });
      // Commits are not journaled operations: checkpoint so a crash keeps them.
      await flushJournal();
      await checkpoint();
      return { commit, state };
    }
    case "versionLog":
      return JSON.parse(runtime.versionLog());
    case "versionDiff":
      return JSON.parse(runtime.versionDiff(m.from, m.to));
    case "restoreVersion": {
      const state = JSON.parse(runtime.restoreVersion(m.hash));
      postMessage({ t: "patch", value: state });
      await flushJournal();
      return state;
    }
    case "saved": {
      const saved = saves.get(m.token);
      if (!saved) throw Error("INVALID_ARGUMENT: save token expired");
      const state = JSON.parse(runtime.markSaved(saved.hash));
      if (state.document.revision === saved.revision) {
        await io("checkpoint", { bytes: saved.bytes.buffer }, [
          saved.bytes.buffer,
        ]);
        journalBacklog = [];
      }
      saves.delete(m.token);
      return state;
    }
    case "open": {
      const s = JSON.parse(runtime.openPackage(new Uint8Array(m.bytes)));
      await io("checkpoint", { bytes: m.bytes }, [m.bytes]);
      journalBacklog = [];
      return s;
    }
    case "recover": {
      const recovery = await io("load");
      if (!recovery) throw Error("NOT_FOUND: recovery journal");
      runtime.openPackage(recovery.base);
      for (const r of recovery.records) runtime.replayRecord(r);
      const s = JSON.parse(runtime.view());
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
    case "waitFrame": {
      let frame;
      do {
        frame = JSON.parse(runtime.tick(8));
        if (frame.pending) await new Promise((r) => setTimeout(r, 0));
      } while (frame.pending);
      await runtime.gpuIdle();
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
            postMessage({ t: "frame", value });
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
  }
}

self.onmessage = ({ data: m }) => {
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
