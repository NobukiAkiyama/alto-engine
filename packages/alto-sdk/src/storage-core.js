// Two alternating, checksummed OPFS journals. A checkpoint becomes visible after flush.
const MAGIC = new TextEncoder().encode("ALTOJ001");
const MAX_RECORD_BYTES = 540 * 1024 * 1024;
let directory,
  active = null,
  candidates = [],
  generation = 0,
  offset = 0;
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
    else if (payload[0] === 2 && base === null) {
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
async function init() {
  if (directory) return;
  directory = await (
    await navigator.storage.getDirectory()
  ).getDirectoryHandle("alto-engine", { create: true });
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
  for (const candidate of found) {
    const meta = candidate.metadata;
    if (meta) {
      if (meta.format !== 2 || !Array.isArray(meta.opaque) ||
          !/^[0-9a-f]{64}$/.test(meta.packageSha256) ||
          meta.opaque.some((hash) => typeof hash !== "string" || !/^[0-9a-f]{64}$/.test(hash)) ||
          await sha256(candidate.base) !== meta.packageSha256) continue;
    }
    candidates.push(candidate);
  }
  candidates.sort((a, b) => b.generation - a.generation);
  if (candidates.length) {
    active = candidates[0];
    offset = active.validLength;
  }
}
async function checkpoint(bytes, opaque) {
  await init();
  const metadata = opaque === null ? null : { format: 2, packageSha256: await sha256(bytes), opaque };
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
    let r;
    if (metadata) {
      const metaBytes = encoder.encode(JSON.stringify(metadata));
      const payload = new Uint8Array(4 + metaBytes.length + bytes.length);
      new DataView(payload.buffer).setUint32(0, metaBytes.length, true);
      payload.set(metaBytes, 4);
      payload.set(bytes, 4 + metaBytes.length);
      r = record(2, payload);
    } else {
      r = record(0, bytes);
    }
    if (h.write(r, { at: 12 }) !== r.length) throw Error("Short checkpoint write");
    h.truncate(12 + r.length);
    h.flush();
    generation++;
    offset = 12 + r.length;
    active = {
      name,
      file,
      generation,
      base: bytes,
      metadata,
      records: [],
      validLength: offset,
      truncated: false,
    };
    candidates = [active, ...candidates.filter((candidate) => candidate.name !== name)];
  } finally {
    h.close();
  }
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
      available: !!active?.records.length,
      records: active?.records.length || 0,
      truncated: !!active?.truncated,
      latestGeneration: generation,
    };
  else if (data.t === "load") {
    value = active
      ? {
          base: active.base,
          metadata: active.metadata,
          records: active.records,
          truncated: active.truncated,
        }
      : null;
  } else if (data.t === "loadCandidates") {
    value = candidates.map((candidate) => ({
      generation: candidate.generation,
      latestGeneration: generation,
      base: candidate.base,
      metadata: candidate.metadata,
      records: candidate.records,
      truncated: candidate.truncated,
    }));
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
    value = true;
  } else if (data.t === "checkpoint") {
    await checkpoint(new Uint8Array(data.bytes), data.opaque || []);
    value = true;
  } else if (data.t === "checkpointLegacy") {
    await checkpoint(new Uint8Array(data.bytes), null);
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
    active.records.push(bytes);
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
