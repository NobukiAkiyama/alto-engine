import type { Args, FrameStats, RecoveryInfo, RequestMessage, StreamMessage, WireError } from "./protocol.js";
import type { StorageMessage } from "./storage-core.js";
// MessageChannel yields to Worker messages without nested timer clamping.
const yieldTask = () => new Promise<void>((resolve) => {
  const channel = new MessageChannel();
  channel.port1.onmessage = () => {
    channel.port1.close(); channel.port2.close(); resolve();
  };
  channel.port2.postMessage(null);
});
import init, { Runtime } from "./pkg/alto_wasm.js";
import { createStorageSession } from "./storage-session.js";
/**
 * Waits for the frame in flight on the GPU, when a frame was held back for it. The
 * timer bounds the wait: a lost device may never settle its queue, and the next
 * tick reports the loss. `gpuIdle` would keep the Runtime borrowed past the timer,
 * and the next tick would fail with "recursive use of an object".
 */
const presentSettled = () => Promise.race([
  runtime.submittedWorkDone(),
  new Promise<void>((resolve) => setTimeout(resolve, 50)),
]);
declare const self: DedicatedWorkerGlobalScope & { altoPanic?: string };
type Timer = ReturnType<typeof setTimeout>;
interface Decoder {
  worker: Worker;
  /** Batch id → timer and bytes, for batches sent and not yet answered. */
  pending: Map<number, { timer: Timer; bytes: number }>;
  ready: boolean;
  memory: number;
}
/** What the drain loop runs in order: requests, and stroke input that has no reply. */
type Queued = RequestMessage | (Extract<StreamMessage, { t: "input" }> & { id?: undefined });
interface StoreState {
  available: boolean;
  records: number;
  truncated: boolean;
  latestGeneration: number;
  objects?: string[];
  packs: number;
}
interface Candidate {
  generation: number;
  latestGeneration: number;
  truncated: boolean;
}
interface CandidateContents {
  base: Uint8Array;
  metadata: { format: number; opaque: string[] } | null;
  records: Uint8Array[];
}
interface Write {
  kind: "append" | "checkpoint";
  bytes: Uint8Array;
  seq?: number;
}
self.addEventListener("error", (e) => {
  postMessage({
    t: "diagnostic",
    value: {
      message: e.message,
      file: e.filename,
      line: e.lineno,
      error: String(e.error),
      // Set by the runtime's panic hook: a Rust panic otherwise reads "unreachable".
      panic: self.altoPanic || null,
    },
  });
});
let diagnosticProfiles: Record<string, number>[] | null = null;
let injectedStorageFault: "beforeAppend" | "afterAppend" | null = null;
const readableAfterFailure = new Set<string>([
  "emergencyExport", "save", "view", "waitFrame", "readTile", "readTileLevel",
  "selectionOutline", "textInfo", "textHitTest", "textCaret",
  "textSelection", "textNavigate", "versionLog", "versionDiff",
  "vectorStats", "vectorPathSvg", "releaseSave", "snapshotHash", "nodeTiles", "exportImage",
]);
let storageFailure: WireError | null = null;
// False when the page started without a recovery journal: nothing is written.
let journaling = true;
function freezeStorage(error: unknown) {
  if (storageFailure) return;
  storageFailure = structured(error);
  writes = [];
  pendingBytes = 0;
  for (const d of durableWaiters.splice(0)) d.reject(Error(storageFailure!.message));
  for (const wake of roomWaiters.splice(0)) wake();
  postMessage({ t: "storageFailure", value: storageFailure });
}
let runtime!: Runtime,
  canvas!: OffscreenCanvas,
  budget = 512,
  queue: Queued[] = [],
  draining = false,
  wantTick = false,
  pendingViewport: Extract<StreamMessage, { t: "viewport" }> | null = null,
  pendingResize: Extract<StreamMessage, { t: "resize" }> | null = null,
  viewVersion = 0,
  frameSettled = false,
  prefetchTimer: Timer | null = null,
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
      pumpDecodes();
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
// Resumable jobs leave input decoding to these Workers, so a cold frame decodes on
// several threads. Without them (nested Workers unavailable, or one failed) the
// engine decodes inline as before.
const DECODE_BATCH = 16;
const DECODES_IN_FLIGHT_PER_WORKER = 2;
// A batch of 16 tiles decodes in a few milliseconds. A decode Worker silent for this
// long is hung; its requests go back to inline decoding.
const DECODE_TIMEOUT_MS = 2000;
let decoders: Decoder[] = [];
let decodersOn = false;
let decodeBatchId = 0;
// Results wait here: a decode Worker answers at any time, possibly while an async
// runtime call (endBrush, readTile) holds the runtime. Only the drain loop and
// waitFrame apply them, between runtime calls.
let decodedResults: ArrayBuffer[] = [];
let decodedResultBytes = 0;
let decodersAllowed = true;
// Highest decode pool memory seen in this Worker's life, sampled at every send and
// answer: frames alone would miss the busiest moment of a cold frame.
let decodePoolPeak = 0;
function notePoolBytes() {
  const { workers, transit } = decodePoolBytes();
  decodePoolPeak = Math.max(decodePoolPeak, workers + transit);
}
function startDecoders() {
  const count = Math.min(3, Math.max(0, (navigator.hardwareConcurrency || 2) - 1));
  for (let i = 0; i < count; i++) {
    let worker;
    try {
      worker = new Worker(new URL("./decode-worker.js", import.meta.url), { type: "module" });
    } catch {
      break;
    }
    // pending: batch id -> { timer, bytes } for batches sent and not yet answered.
    const decoder: Decoder = { worker, pending: new Map(), ready: false, memory: 0 };
    worker.onmessage = ({ data }) => {
      decoder.memory = data.memory ?? decoder.memory;
      if (data.ready) decoder.ready = true;
      else {
        const sent = decoder.pending.get(data.id);
        // An answer to a batch this pool no longer waits for is dropped.
        if (!sent || !decoders.includes(decoder)) return;
        clearTimeout(sent.timer);
        decoder.pending.delete(data.id);
        decodedResults.push(data.out);
        decodedResultBytes += data.out.byteLength;
      }
      notePoolBytes();
      wantTick = true;
      void drain();
    };
    worker.onerror = () => stopDecoders("decode worker failed");
    decoders.push(decoder);
  }
}
/**
 * Back to inline decoding. Never touches the runtime (a timer may call this while an
 * async runtime call is running); the next pumpDecodes tells the engine to forget its
 * outstanding requests, and the jobs that asked for them decode them inline.
 */
function stopDecoders(why?: string) {
  for (const decoder of decoders) {
    for (const { timer } of decoder.pending.values()) clearTimeout(timer);
    decoder.worker.terminate();
  }
  decoders = [];
  decodedResults = [];
  decodedResultBytes = 0;
  if (why) postMessage({ t: "diagnostic", value: { message: "decode workers stopped: " + why } });
  wantTick = true;
  void drain();
}
/** Bytes the decode pool holds: its Workers' WASM memory and batches in transit. */
function decodePoolBytes() {
  let workers = 0;
  let transit = decodedResultBytes;
  for (const decoder of decoders) {
    workers += decoder.memory;
    for (const { bytes } of decoder.pending.values()) transit += bytes;
  }
  return { workers, transit };
}
/** Only between runtime calls: applies results, then hands out requests. */
function pumpDecodes() {
  if (!runtime) return;
  const ready = decodersAllowed && decoders.some((d) => d.ready);
  if (ready !== decodersOn) {
    decodersOn = ready;
    runtime.setDecodeWorkers(ready);
  }
  for (const out of decodedResults.splice(0)) {
    try {
      if (decodersOn) runtime.provideDecoded(new Uint8Array(out));
    } catch (error) {
      postMessage({ t: "error", value: structured(error) });
    }
  }
  decodedResultBytes = 0;
  if (!decodersOn) return;
  for (const decoder of decoders) {
    while (decoder.ready && decoder.pending.size < DECODES_IN_FLIGHT_PER_WORKER) {
      const batch = runtime.takeDecodeRequests(DECODE_BATCH);
      if (!batch.length) return;
      const id = ++decodeBatchId;
      const timer = setTimeout(
        () => stopDecoders(`a decode worker did not answer batch ${id} within ${DECODE_TIMEOUT_MS} ms`),
        DECODE_TIMEOUT_MS,
      );
      decoder.pending.set(id, { timer, bytes: batch.byteLength });
      decoder.worker.postMessage({ id, batch: batch.buffer }, [batch.buffer]);
      notePoolBytes();
    }
  }
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
/** A storage request; results are typed where they are used. */
async function io<T = unknown>(
  t: StorageMessage["t"],
  payload: Omit<StorageMessage, "t"> = {},
  transfer: Transferable[] = [],
): Promise<T> {
  try {
    return (await journal.request(t, payload as Record<string, unknown>, transfer)) as T;
  } catch (error) {
    freezeStorage(error);
    throw error;
  }
}
function structured(error: unknown): WireError {
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
    // serde rejects a malformed request before the engine sees it.
    (/ at line \d+ column \d+/.test(message) ? "INVALID_ARGUMENT" : "INTERNAL");
  return { code, message };
}
// Journal and checkpoint writes run in order on their own loop, so neither an edit
// nor a frame waits for OPFS. An edit waits only when the backlog is over its limit.
// The runtime may be ahead of the journal; durable() reports when it caught up.
const MAX_PENDING_BYTES = 64 * 1024 * 1024;
const MAX_PENDING_WRITES = 256;
let writes: Write[] = [];
let pendingBytes = 0;
let queuedSeq = 0;
let durableSeq = 0;
let pumping: Promise<void> | null = null;
let durableWaiters: { seq: number; resolve(): void; reject(e: Error): void }[] = [];
let roomWaiters: (() => void)[] = [];
// A long session without saves or commits would grow the journal without bound and
// make recovery replay all of it. Past these, checkpoint once the user pauses.
const AUTO_CHECKPOINT_BYTES = 32 * 1024 * 1024;
const AUTO_CHECKPOINT_RECORDS = 500;
const AUTO_CHECKPOINT_IDLE_MS = 2000;
let lastCommandAt = 0;
let journaledBytes = 0;
let journaledRecords = 0;
let autoCheckpointTimer: Timer | null = null;
function scheduleAutoCheckpoint() {
  if (autoCheckpointTimer !== null) return;
  // Fire when the pause would be long enough, not a fixed interval after the last check.
  const wait = Math.max(0, lastCommandAt + AUTO_CHECKPOINT_IDLE_MS - performance.now()) + 10;
  autoCheckpointTimer = setTimeout(() => {
    autoCheckpointTimer = null;
    if (storageFailure || !runtime) return;
    // The page sends a tick every animation frame, so only commands count as activity.
    const busy = draining || queue.length || performance.now() - lastCommandAt < AUTO_CHECKPOINT_IDLE_MS;
    if (busy) scheduleAutoCheckpoint();
    else {
      // A timer is outside the drain loop's error handling; a failure here must not
      // take the Worker down. The journal still holds every edit.
      try {
        queueCheckpoint();
      } catch (error) {
        freezeStorage(Error("STORAGE: automatic checkpoint failed: " + String((error as Error).message || error)));
      }
    }
  }, wait);
}
function enqueue(entry: Write) {
  if (!journaling) return;
  entry.seq = ++queuedSeq;
  writes.push(entry);
  pendingBytes += entry.bytes.length;
  if (!pumping) pumping = pump().finally(() => (pumping = null));
}
function queueJournal() {
  let bytes;
  while ((bytes = runtime.takeJournal()).length) enqueue({ kind: "append", bytes });
}
/**
 * Checkpoints the current document: its package without the objects the store already
 * holds. It contains every edit queued so far, so their appends are dropped.
 */
function queueCheckpoint() {
  if (!journaling) return;
  journaledBytes = 0;
  journaledRecords = 0;
  const bytes = runtime.checkpointParts();
  for (const w of writes) pendingBytes -= w.bytes.length;
  writes = writes.filter((w) => w.kind !== "append");
  for (const w of writes) pendingBytes += w.bytes.length;
  enqueue({ kind: "checkpoint", bytes });
}
async function write(w: Write) {
  if (w.kind === "checkpoint") {
    await io("checkpoint", { entries: w.bytes.buffer as ArrayBuffer }, [w.bytes.buffer]);
    return;
  }
  if (injectedStorageFault === "beforeAppend") {
    injectedStorageFault = null;
    throw Error("STORAGE: injected failure before append");
  }
  await io("append", { bytes: w.bytes.buffer as ArrayBuffer }, [w.bytes.buffer]);
  if (injectedStorageFault === "afterAppend") {
    injectedStorageFault = null;
    throw Error("STORAGE: injected lost reply after append");
  }
}
async function pump() {
  while (writes.length && !storageFailure) {
    const w = writes.shift()!;
    // Writing transfers the buffer; read its size first.
    const size = w.bytes.length;
    pendingBytes -= size;
    try {
      await write(w);
    } catch (error) {
      freezeStorage(error);
      return;
    }
    durableSeq = w.seq!;
    if (w.kind === "append") {
      journaledBytes += size;
      journaledRecords++;
      if (journaledBytes >= AUTO_CHECKPOINT_BYTES || journaledRecords >= AUTO_CHECKPOINT_RECORDS)
        scheduleAutoCheckpoint();
    }
    durableWaiters = durableWaiters.filter((d) => (d.seq <= durableSeq ? (d.resolve(), false) : true));
    if (hasRoom()) for (const wake of roomWaiters.splice(0)) wake();
  }
}
function hasRoom() {
  return writes.length < MAX_PENDING_WRITES && pendingBytes < MAX_PENDING_BYTES;
}
function waitForRoom() {
  return hasRoom() || storageFailure ? null : new Promise<void>((resolve) => roomWaiters.push(resolve));
}
/** Resolves once everything queued so far is in OPFS; rejects if storage failed. */
function durable() {
  if (storageFailure) return Promise.reject(Error(storageFailure.message));
  if (durableSeq >= queuedSeq) return Promise.resolve();
  return new Promise<void>((resolve, reject) => durableWaiters.push({ seq: queuedSeq, resolve, reject }));
}
/** Length-prefixed records, so recovery crosses into WASM once. */
function frameRecords(records: Uint8Array[]) {
  const out = new Uint8Array(records.reduce((n, r) => n + 4 + r.length, 0));
  const view = new DataView(out.buffer);
  let at = 0;
  for (const r of records) {
    view.setUint32(at, r.length, true);
    out.set(r, at + 4);
    at += 4 + r.length;
  }
  return out;
}
/** Hooks that read or damage OPFS directly run after every queued write. */
async function writesIdle() {
  while (pumping) await pumping;
}
// tick() returns numbers in the order frameFields() names them, fixed for the build.
let frameFields: string[] = [];
let adapterName = "";
function decodeFrame(numbers: Float64Array): FrameStats {
  const pool = decodePoolBytes();
  // wasmBytes is the engine runtime alone; the decode pool is reported beside it.
  const prefetch: Record<string, number> = {};
  const out: Record<string, unknown> & { profile: Record<string, number> | null; hasProfile?: number } = {
    adapter: adapterName,
    prefetch,
    profile: null,
    decodeWorkerBytes: pool.workers,
    decodeTransitBytes: pool.transit,
    decodePeakBytes: decodePoolPeak,
    // The drawing buffer, in device pixels.
    surface: [canvas.width, canvas.height],
  };
  const profile: Record<string, number> = {};
  frameFields.forEach((name, i) => {
    const [group, key] = name.split(".");
    if (key === undefined) out[group] = numbers[i];
    else (group === "profile" ? profile : prefetch)[key] = numbers[i];
  });
  if (out.hasProfile) out.profile = profile;
  delete out.hasProfile;
  return out as FrameStats;
}
/** Display tiles read back per GPU round trip. */
const EXPORT_BATCH = 64;
/**
 * The document as an image file, from the same display tiles the canvas shows. A
 * reduced export reads the pyramid level at or above its scale: each level halves the
 * tiles to read. Tiles are premultiplied; image files hold straight alpha.
 */
async function exportImage({ region, scale, type }: Args<"exportImage">) {
  const level = Math.max(0, Math.min(12, Math.floor(Math.log2(1 / scale) + 1e-9)));
  const f = 2 ** level;
  const x0 = Math.floor(region.x0 / f), y0 = Math.floor(region.y0 / f);
  const x1 = Math.ceil(region.x1 / f), y1 = Math.ceil(region.y1 / f);
  const w = x1 - x0, h = y1 - y0;
  if (w * h > 64_000_000) throw Error("INVALID_ARGUMENT: export exceeds 64M pixels");
  const pixels = new Uint8ClampedArray(w * h * 4);
  const tiles: [number, number][] = [];
  for (let ty = Math.floor(y0 / 256); ty * 256 < y1; ty++)
    for (let tx = Math.floor(x0 / 256); tx * 256 < x1; tx++) tiles.push([tx, ty]);
  for (let at = 0; at < tiles.length; at += EXPORT_BATCH) {
    const batch = tiles.slice(at, at + EXPORT_BATCH);
    const bytes = await runtime.readTiles(level, Int32Array.from(batch.flat()));
    batch.forEach(([tx, ty], i) => {
      const tile = bytes.subarray(i * 262144, (i + 1) * 262144);
      const left = Math.max(x0, tx * 256), right = Math.min(x1, tx * 256 + 256);
      for (let y = Math.max(y0, ty * 256); y < Math.min(y1, ty * 256 + 256); y++)
        for (let x = left; x < right; x++) {
          const s = ((y - ty * 256) * 256 + (x - tx * 256)) * 4, d = ((y - y0) * w + (x - x0)) * 4;
          const a = tile[s + 3];
          pixels[d + 3] = a;
          if (a) for (let c = 0; c < 3; c++) pixels[d + c] = Math.round((tile[s + c] * 255) / a);
        }
    });
  }
  const full = new OffscreenCanvas(w, h);
  full.getContext("2d")!.putImageData(new ImageData(pixels, w, h), 0, 0);
  // The region in level pixels, and the size the caller asked for.
  const sx = region.x0 / f - x0, sy = region.y0 / f - y0;
  const sw = (region.x1 - region.x0) / f, sh = (region.y1 - region.y0) / f;
  const ow = Math.max(1, Math.round((region.x1 - region.x0) * scale));
  const oh = Math.max(1, Math.round((region.y1 - region.y0) * scale));
  if (sx === 0 && sy === 0 && ow === w && oh === h) return full.convertToBlob({ type });
  const out = new OffscreenCanvas(ow, oh);
  const ctx = out.getContext("2d")!;
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(full, sx, sy, sw, sh, 0, 0, ow, oh);
  return out.convertToBlob({ type });
}
let savedToken = 0;
// The state each save wrote, until the page confirms or releases it.
const saves = new Map<number, { hash: string; revision: number }>();
const MAX_PENDING_SAVES = 4;
function rememberSave(entry: { hash: string; revision: number }) {
  const token = ++savedToken;
  saves.set(token, entry);
  // A caller that never confirms (or crashed mid-save) must not pin packages forever.
  while (saves.size > MAX_PENDING_SAVES) saves.delete(saves.keys().next().value!);
  return token;
}
async function execute(m: Queued): Promise<unknown> {
  // The in-memory runtime may be ahead of the last confirmed journal write.
  // Keep it available for an emergency export, but accept no more edits.
  if (storageFailure && !readableAfterFailure.has(m.t))
    throw Error("STORAGE: the journal cannot confirm writes; export the document, then recover");
  if (!readableAfterFailure.has(m.t)) await waitForRoom();
  switch (m.t) {
    case "init":
      if (m.protocol !== 4) throw Error("CONFLICT: protocol mismatch");
      canvas = m.canvas;
      budget = m.budget;
      await init();
      runtime = await Runtime.create(canvas, budget);
      frameFields = runtime.frameFields();
      adapterName = runtime.adapterName();
      startDecoders();
      if (runtime.protocolVersion?.() !== 4)
        throw Error("CONFLICT: SDK/WASM version mismatch");
      journaling = m.journal !== false;
      if (!journaling) return { protocol: 4, recovery: { available: false, records: 0, latestGeneration: 0 } };
      await io("configure", { namespace: m.namespace || "default" });
      const recovery = await io<StoreState>("inspect");
      // Later checkpoints skip the objects the store already holds.
      runtime.noteStoredObjects(JSON.stringify(recovery.objects));
      delete recovery.objects;
      const info: RecoveryInfo = recovery;
      return { protocol: 4, recovery: info };
    case "discardRecovery":
      if (!journaling) return true;
      await writesIdle();
      await io("discard");
      runtime.forgetStoredObjects();
      return true;
    case "createDocument": {
      const state = JSON.parse(runtime.createDocument(m.width, m.height));
      queueCheckpoint();
      return state;
    }
    case "transact": {
      const s = JSON.parse(
        runtime.transact(
          JSON.stringify({
            intent: m.intent,
            label: m.label,
            ops: m.ops,
            baseRevision: m.baseRevision,
          }),
        ),
      );
      queueJournal();
      postMessage({ t: "patch", value: s });
      return s;
    }
    case "undo":
    case "redo": {
      const s = JSON.parse(runtime[m.t]());
      queueJournal();
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
      queueJournal();
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
      queueJournal();
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
      const token = rememberSave({
        hash: state.stateHash,
        revision: state.document.revision,
      });
      return { bytes, token };
    }
    case "folderEntries": {
      const state = JSON.parse(runtime.view());
      const bytes = runtime.folderEntries();
      const token = rememberSave({
        hash: state.stateHash,
        revision: state.document.revision,
      });
      return { bytes, token };
    }
    case "folderPlan":
      return JSON.parse(runtime.folderPlan(new Uint8Array(m.bytes)));
    case "openFolder": {
      const s = JSON.parse(runtime.openFolder(new Uint8Array(m.bytes)));
      queueCheckpoint();
      return s;
    }
    case "importFont":
      return JSON.parse(runtime.importFont(new Uint8Array(m.bytes), m.faceIndex));
    case "releaseImport":
      return runtime.releaseImport(m.hash);
    case "pruneVersions": {
      const state = JSON.parse(runtime.pruneVersions());
      queueJournal();
      queueCheckpoint();
      postMessage({ t: "patch", value: state });
      return state;
    }
    case "snapshotHash":
      return runtime.snapshotHash();
    case "nodeTiles":
      return JSON.parse(runtime.nodeTiles(m.node));
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
      queueJournal();
      queueCheckpoint();
      postMessage({ t: "patch", value: state });
      return { commit, state };
    }
    case "versionLog":
      return JSON.parse(runtime.versionLog());
    case "versionDiff":
      return JSON.parse(runtime.versionDiff(m.from, m.to));
    case "restoreVersion": {
      const state = JSON.parse(runtime.restoreVersion(m.hash));
      queueJournal();
      postMessage({ t: "patch", value: state });
      return state;
    }
    case "saved": {
      const saved = saves.get(m.token);
      if (!saved) throw Error("INVALID_ARGUMENT: save token expired");
      const state = JSON.parse(runtime.markSaved(saved.hash));
      if (state.document.revision === saved.revision) {
        queueCheckpoint();
      }
      saves.delete(m.token);
      return state;
    }
    case "releaseSave":
      return saves.delete(m.token);
    case "open": {
      const s = JSON.parse(runtime.openPackage(new Uint8Array(m.bytes)));
      queueCheckpoint();
      return s;
    }
    case "recover": {
      if (!journaling) throw Error("NOT_FOUND: the engine keeps no recovery journal");
      await writesIdle();
      const candidates = await io<Candidate[]>("loadCandidates");
      if (!candidates.length) {
        const state = await io<StoreState>("inspect");
        throw Error(state.latestGeneration
          ? "CORRUPT_DATA: no usable recovery generation"
          : "NOT_FOUND: recovery journal");
      }
      let recovery: Candidate | null = null;
      let lastError: unknown;
      for (const candidate of candidates) {
        try {
          // A candidate that cannot be read (a missing object) is skipped like one that
          // cannot be opened; it does not stop storage.
          const full = (await journal.request("loadCandidate", { generation: candidate.generation })) as CandidateContents;
          if (full.metadata && full.metadata.format >= 3)
            runtime.openRecoveryEntries(full.base, JSON.stringify(full.metadata.opaque));
          else if (full.metadata)
            runtime.openRecoveryPackage(full.base, JSON.stringify(full.metadata.opaque));
          else runtime.openPackage(full.base);
          runtime.replayRecords(frameRecords(full.records));
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
        s.warnings.push({ code: "RECOVERED_OLDER_GENERATION", details: {} });
      if (recovery.truncated)
        s.warnings.push({ code: "RECOVERED_TRUNCATED", details: {} });
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
    case "recoveryStore": {
      await writesIdle();
      const store = await io<StoreState>("inspect");
      return {
        latestGeneration: store.latestGeneration,
        records: store.records,
        objects: store.objects!.length,
        packs: store.packs,
      };
    }
    case "corruptJournalTail":
      await writesIdle();
      return io("corruptTail");
    case "corruptCheckpoint":
      await writesIdle();
      return io("corruptCheckpoint");
    case "corruptCheckpointCrc":
      await writesIdle();
      return io("corruptCheckpointCrc");
    case "corruptBothCheckpoints":
      await writesIdle();
      return io("corruptBothCheckpoints");
    case "looseCheckpoint": {
      await writesIdle();
      const entries = runtime.checkpointParts();
      await io("checkpointLoose", { entries: entries.buffer as ArrayBuffer }, [entries.buffer]);
      return true;
    }
    case "legacyCheckpoint": {
      await writesIdle();
      const bytes = runtime.savePackage();
      await io("checkpointLegacy", { bytes: bytes.buffer as ArrayBuffer }, [bytes.buffer]);
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
    case "exportImage":
      return exportImage(m);
    case "destroyGpu":
      runtime.destroyGpu();
      return true;
    case "setDecodeWorkers":
      // Diagnostic: compare inline decoding with the pool. Enabling after the pool
      // stopped starts a new one; it is used once a Worker reports ready.
      decodersAllowed = !!m.enabled;
      if (decodersAllowed && !decoders.length) startDecoders();
      pumpDecodes();
      return decodersOn;
    case "stallDecodeWorkers":
      // Fault injection: the decode Workers keep requests and never answer.
      for (const decoder of decoders) decoder.worker.postMessage({ stall: true });
      return decoders.length;
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
        pumpDecodes();
        frame = decodeFrame(runtime.tick(8));
        pumpDecodes();
        if (frame.profile) profiles.push(frame.profile);
        if (frame.pending) await yieldTask();
        else if (frame.presentDeferred) await presentSettled();
      } while (frame.pending || frame.presentDeferred);
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
      pumpDecodes();
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
        const m = queue.shift()!;
        try {
          const value = await execute(m);
          const bytes = (value as { bytes?: unknown } | null)?.bytes;
          if (m.id)
            postMessage(
              { t: "reply", id: m.id, ok: true, value },
              bytes instanceof Uint8Array ? [bytes.buffer] : value instanceof Uint8Array ? [value.buffer] : [],
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
            const value = { ...decodeFrame(runtime.tick(8)), viewVersion };
            pumpDecodes();
            if (value.profile && diagnosticProfiles) diagnosticProfiles.push(value.profile);
            frameSettled = value.pending === 0 && !value.presentDeferred;
            if (!frameSettled) wantTick = true;
            if (frameSettled) prefetchSpentMs = 0;
            postMessage({ t: "frame", value });
            // A held-back frame with nothing new composed waits for the GPU instead of
            // spinning: on a software adapter, spinning takes the GPU's CPU time.
            if (value.presentDeferred && !value.composed) await presentSettled();
            else if (value.pending) await yieldTask();
          } catch (error) {
            if (String(error).includes("GPU_LOST")) {
              postMessage({ t: "gpuLost" });
              try {
                await runtime.recoverGpu();
                adapterName = runtime.adapterName();
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

self.onmessage = ({ data: m }: MessageEvent<Queued | StreamMessage | { t: "durable"; id: number }>) => {
  if (m.t === "durable") {
    // Answered beside the queue: waiting for OPFS must not hold back edits or frames.
    durable().then(
      () => postMessage({ t: "reply", id: m.id, ok: true, value: true }),
      (error) => postMessage({ t: "reply", id: m.id, ok: false, error: structured(error) }),
    );
    return;
  }
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
  } else {
    lastCommandAt = performance.now();
    queue.push(m);
  }
  void drain();
};
