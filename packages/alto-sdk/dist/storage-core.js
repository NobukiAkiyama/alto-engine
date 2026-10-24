// Two alternating, checksummed OPFS journals. A checkpoint becomes visible after flush.
// Format 3 checkpoints keep objects once each under objects/, named by their hash, and
// write only the objects the store lacks. Earlier formats hold a whole package and
// are still read, so the next checkpoint migrates them.
const MAGIC = new TextEncoder().encode("ALTOJ001");
const MAX_RECORD_BYTES = 540 * 1024 * 1024;
const OBJECT_PATH = /^objects\/([0-9a-f]{2})\/([0-9a-f]{64})$/;
let directory,
  objectsDirectory,
  active = null,
  candidates = [],
  generation = 0,
  offset = 0,
  // Object paths on disk that a valid checkpoint references. Deleted only at startup:
  // a checkpoint skips objects listed here, so one must never vanish mid-session.
  stored = new Set(),
  // Parsed checkpoints and records stay in memory only until recovery is decided.
  released = false;
const encoder = new TextEncoder();
const decoder = new TextDecoder();
async function sha256(bytes) {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let i = 0; i < 8; i++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc(bytes) {
  let c = 0xffffffff;
  for (const b of bytes) c = crcTable[(c ^ b) & 255] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function record(tag, bytes) {
  const payload = new Uint8Array(bytes.length + 1);
  payload[0] = tag;
  payload.set(bytes, 1);
  const out = new Uint8Array(payload.length + 8);
  const v = new DataView(out.buffer);
  v.setUint32(0, payload.length, true);
  v.setUint32(4, crc(payload), true);
  out.set(payload, 8);
  return out;
}
/** Entries framed as the engine frames them: count, then (name length, name, data length, data). */
function frame(entries) {
  const names = [...entries.keys()].map((k) => encoder.encode(k));
  let size = 4;
  let i = 0;
  for (const v of entries.values()) size += 8 + names[i++].length + v.length;
  const out = new Uint8Array(size);
  const view = new DataView(out.buffer);
  view.setUint32(0, entries.size, true);
  let p = 4;
  i = 0;
  for (const v of entries.values()) {
    const n = names[i++];
    view.setUint32(p, n.length, true);
    out.set(n, p + 4);
    p += 4 + n.length;
    view.setUint32(p, v.length, true);
    out.set(v, p + 4);
    p += 4 + v.length;
  }
  return out;
}
function unframe(b) {
  const view = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const out = new Map();
  let p = 4;
  for (let i = 0, n = view.getUint32(0, true); i < n; i++) {
    const l = view.getUint32(p, true);
    const name = decoder.decode(b.subarray(p + 4, p + 4 + l));
    p += 4 + l;
    const d = view.getUint32(p, true);
    out.set(name, b.subarray(p + 4, p + 4 + d));
    p += 4 + d;
  }
  return out;
}
function parse(bytes) {
  if (bytes.length < 12 || !MAGIC.every((b, i) => bytes[i] === b)) return null;
  let p = 12,
    base = null,
    metadata = null,
    records = [],
    truncated = false;
  while (p < bytes.length) {
    if (p + 8 > bytes.length) {
      truncated = true;
      break;
    }
    const v = new DataView(bytes.buffer, bytes.byteOffset + p, 8);
    const n = v.getUint32(0, true);
    if (!n || n > MAX_RECORD_BYTES || p + 8 + n > bytes.length) {
      truncated = true;
      break;
    }
    const payload = bytes.subarray(p + 8, p + 8 + n);
    if (crc(payload) !== v.getUint32(4, true)) {
      truncated = true;
      break;
    }
    if (payload[0] === 0 && base === null) base = payload.slice(1);
    else if ((payload[0] === 2 || payload[0] === 3) && base === null) {
      if (payload.length < 5) { truncated = true; break; }
      const metaLength = new DataView(payload.buffer, payload.byteOffset + 1, 4).getUint32(0, true);
      if (metaLength > 4 * 1024 * 1024 || 5 + metaLength >= payload.length) {
        truncated = true;
        break;
      }
      try {
        metadata = JSON.parse(decoder.decode(payload.subarray(5, 5 + metaLength)));
      } catch {
        truncated = true;
        break;
      }
      base = payload.slice(5 + metaLength);
    }
    else if (payload[0] === 1 && base !== null) records.push(payload.slice(1));
    else {
      truncated = true;
      break;
    }
    p += 8 + n;
  }
  return base
    ? {
        generation: new DataView(
          bytes.buffer,
          bytes.byteOffset + 8,
          4,
        ).getUint32(0, true),
        base,
        metadata,
        records,
        truncated,
        validLength: p,
      }
    : null;
}
const isHash = (hash) => typeof hash === "string" && /^[0-9a-f]{64}$/.test(hash);
async function validMetadata(candidate, present) {
  const meta = candidate.metadata;
  if (!meta) return true;
  if (!Array.isArray(meta.opaque) || meta.opaque.some((hash) => !isHash(hash))) return false;
  if (meta.format === 2)
    return isHash(meta.packageSha256) && (await sha256(candidate.base)) === meta.packageSha256;
  if (meta.format !== 3) return false;
  return (
    isHash(meta.entriesSha256) &&
    Array.isArray(meta.objects) &&
    meta.objects.every((path) => typeof path === "string" && OBJECT_PATH.test(path) && present.has(path)) &&
    (await sha256(candidate.base)) === meta.entriesSha256
  );
}
async function listObjects() {
  const present = new Map();
  for await (const [shard, dir] of objectsDirectory.entries()) {
    if (dir.kind !== "directory") continue;
    for await (const [name] of dir.entries()) {
      const path = `objects/${shard}/${name}`;
      if (OBJECT_PATH.test(path)) present.set(path, dir);
    }
  }
  return present;
}
async function init() {
  if (directory) return;
  directory = await (
    await navigator.storage.getDirectory()
  ).getDirectoryHandle("alto-engine", { create: true });
  objectsDirectory = await directory.getDirectoryHandle("objects", { create: true });
  await scan();
}
/** Reads both journals, keeps the valid checkpoints, and deletes unreferenced objects. */
async function scan() {
  const present = await listObjects();
  const found = [];
  const seenGenerations = [];
  for (const name of ["journal-a.bin", "journal-b.bin"])
    try {
      const f = await directory.getFileHandle(name);
      const bytes = new Uint8Array(await (await f.getFile()).arrayBuffer());
      if (bytes.length >= 12 && MAGIC.every((b, i) => bytes[i] === b))
        seenGenerations.push(new DataView(bytes.buffer, bytes.byteOffset + 8, 4).getUint32(0, true));
      const parsed = parse(bytes);
      if (parsed) found.push({ name, file: f, ...parsed });
    } catch {}
  generation = Math.max(0, ...seenGenerations);
  candidates = [];
  for (const candidate of found)
    if (await validMetadata(candidate, present)) {
      candidate.recordCount = candidate.records.length;
      candidates.push(candidate);
    }
  candidates.sort((a, b) => b.generation - a.generation);
  // An object no valid checkpoint names was written for a checkpoint that never
  // completed, or by a generation since replaced. It may be partial; it is not kept.
  stored = new Set(candidates.flatMap((c) => (c.metadata?.format === 3 ? c.metadata.objects : [])));
  for (const [path, dir] of present)
    if (!stored.has(path)) await dir.removeEntry(path.split("/")[2]).catch(() => {});
  released = false;
  if (candidates.length) {
    active = candidates[0];
    offset = active.validLength;
  }
}
/** After recovery is decided, the parsed packages and records are not needed in memory. */
function release() {
  for (const candidate of candidates) {
    candidate.base = null;
    candidate.records = null;
  }
  released = true;
}
async function readObject(path) {
  const [, shard, name] = path.split("/");
  const dir = await objectsDirectory.getDirectoryHandle(shard);
  const file = await dir.getFileHandle(name);
  return new Uint8Array(await (await file.getFile()).arrayBuffer());
}
async function writeObject(path, bytes) {
  const [, shard, name] = path.split("/");
  const dir = await objectsDirectory.getDirectoryHandle(shard, { create: true });
  const file = await dir.getFileHandle(name, { create: true });
  const h = await file.createSyncAccessHandle();
  try {
    h.truncate(0);
    if (h.write(bytes, { at: 0 }) !== bytes.length) throw Error("Short object write");
    h.flush();
  } finally {
    h.close();
  }
}
/** The package a candidate recovers from: format 3 rejoins its entries and objects. */
async function packageOf(candidate) {
  if (candidate.metadata?.format !== 3) return candidate.base;
  const entries = unframe(candidate.base);
  for (const path of candidate.metadata.objects) entries.set(path, await readObject(path));
  return frame(entries);
}
async function writeGeneration(tag, payload, metadata) {
  const name =
    active?.name === "journal-a.bin" ? "journal-b.bin" : "journal-a.bin";
  const file = await directory.getFileHandle(name, { create: true });
  const h = await file.createSyncAccessHandle();
  try {
    const header = new Uint8Array(12);
    header.set(MAGIC);
    new DataView(header.buffer).setUint32(8, generation + 1, true);
    h.truncate(0);
    h.write(header, { at: 0 });
    const r = record(tag, payload);
    if (h.write(r, { at: 12 }) !== r.length) throw Error("Short checkpoint write");
    h.truncate(12 + r.length);
    h.flush();
    generation++;
    offset = 12 + r.length;
    active = {
      name,
      file,
      generation,
      base: null,
      metadata,
      records: null,
      recordCount: 0,
      validLength: offset,
      truncated: false,
    };
    candidates = [active, ...candidates.filter((candidate) => candidate.name !== name)];
  } finally {
    h.close();
  }
}
function withMetadata(metadata, bytes) {
  const metaBytes = encoder.encode(JSON.stringify(metadata));
  const payload = new Uint8Array(4 + metaBytes.length + bytes.length);
  new DataView(payload.buffer).setUint32(0, metaBytes.length, true);
  payload.set(metaBytes, 4);
  payload.set(bytes, 4 + metaBytes.length);
  return payload;
}
/**
 * Format 3: `framed` is the engine's checkpoint. Objects missing from the store are
 * written and flushed before the journal that names them.
 */
async function checkpoint(framed) {
  await init();
  const entries = unframe(framed);
  const meta = JSON.parse(decoder.decode(entries.get("checkpoint.json")));
  entries.delete("checkpoint.json");
  for (const [path, bytes] of entries) {
    if (!OBJECT_PATH.test(path)) continue;
    if (!stored.has(path)) await writeObject(path, bytes);
    stored.add(path);
    entries.delete(path);
  }
  const missing = meta.objects.filter((path) => !stored.has(path));
  if (missing.length) throw Error("checkpoint names objects the store lacks: " + missing.slice(0, 3).join(", "));
  const base = frame(entries);
  const metadata = {
    format: 3,
    entriesSha256: await sha256(base),
    objects: meta.objects,
    opaque: meta.opaque,
  };
  await writeGeneration(3, withMetadata(metadata, base), metadata);
  release();
}
/** Format 1, kept so tests can produce a journal an older engine wrote. */
async function checkpointLegacy(bytes) {
  await init();
  await writeGeneration(0, bytes, null);
  release();
}
async function damageCheckpoint(candidate, preserveRecordCrc) {
  if (!candidate) throw Error("Journal has no checkpoint");
  const bytes = new Uint8Array(await (await candidate.file.getFile()).arrayBuffer());
  const recordLength = new DataView(bytes.buffer, 12, 4).getUint32(0, true);
  const metaLength = candidate.metadata
    ? new DataView(bytes.buffer, 21, 4).getUint32(0, true)
    : 0;
  const objectOffset = candidate.metadata ? 25 + metaLength : 21;
  if (objectOffset >= 20 + recordLength) throw Error("Checkpoint has no package");
  bytes[objectOffset] ^= 1;
  if (preserveRecordCrc)
    new DataView(bytes.buffer).setUint32(16, crc(bytes.subarray(20, 20 + recordLength)), true);
  const h = await candidate.file.createSyncAccessHandle();
  try {
    if (preserveRecordCrc) h.write(bytes.subarray(16, 20), { at: 16 });
    h.write(bytes.subarray(objectOffset, objectOffset + 1), { at: objectOffset });
    h.flush();
  } finally {
    h.close();
  }
}

/** Handles one journal request; shared by the Storage Worker and the inline fallback. */
export async function handle(data) {
  await init();
  let value;
  if (data.t === "inspect")
    value = {
      available: !!active?.recordCount,
      records: active?.recordCount || 0,
      truncated: !!active?.truncated,
      latestGeneration: generation,
      objects: [...stored],
    };
  else if (data.t === "loadCandidates") {
    if (released) await scan();
    value = [];
    for (const candidate of candidates)
      value.push({
        generation: candidate.generation,
        latestGeneration: generation,
        base: await packageOf(candidate),
        metadata: candidate.metadata && { format: candidate.metadata.format, opaque: candidate.metadata.opaque },
        records: candidate.records,
        truncated: candidate.truncated,
      });
  } else if (data.t === "activate") {
    const selected = candidates.find((candidate) => candidate.generation === data.generation);
    if (!selected) throw Error("Journal generation unavailable");
    active = selected;
    offset = active.validLength;
    if (active.truncated) {
      const h = await active.file.createSyncAccessHandle();
      try {
        h.truncate(offset);
        h.flush();
      } finally {
        h.close();
      }
      active.truncated = false;
    }
    release();
    value = true;
  } else if (data.t === "checkpoint") {
    await checkpoint(new Uint8Array(data.entries));
    value = true;
  } else if (data.t === "checkpointLegacy") {
    await checkpointLegacy(new Uint8Array(data.bytes));
    value = true;
  } else if (data.t === "append") {
    if (!active) throw Error("Journal has no checkpoint");
    const bytes = new Uint8Array(data.bytes);
    const r = record(1, bytes);
    const h = await active.file.createSyncAccessHandle();
    try {
      const written = h.write(r, { at: offset });
      if (written !== r.length) throw Error("Short journal write");
      h.flush();
    } finally {
      h.close();
    }
    offset += r.length;
    active.recordCount++;
    value = true;
  } else if (data.t === "corruptTail") {
    if (!active) throw Error("Journal has no checkpoint");
    const h = await active.file.createSyncAccessHandle();
    try {
      h.write(new Uint8Array([10, 0, 0]), { at: offset });
      h.flush();
    } finally {
      h.close();
    }
    value = true;
  } else if (data.t === "corruptCheckpoint" || data.t === "corruptCheckpointCrc") {
    await damageCheckpoint(active, data.t === "corruptCheckpoint");
    value = true;
  } else if (data.t === "corruptBothCheckpoints") {
    for (const candidate of candidates) await damageCheckpoint(candidate, true);
    value = true;
  }
  if (data.t === "ping") value = true;
  return value;
}
