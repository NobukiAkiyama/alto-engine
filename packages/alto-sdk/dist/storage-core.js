// Two alternating, checksummed OPFS journals. A checkpoint becomes visible after flush.
const MAGIC = new TextEncoder().encode("ALTOJ001");
let directory,
  active = null,
  generation = 0,
  offset = 0;
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
    records = [],
    truncated = false;
  while (p < bytes.length) {
    if (p + 8 > bytes.length) {
      truncated = true;
      break;
    }
    const v = new DataView(bytes.buffer, bytes.byteOffset + p, 8);
    const n = v.getUint32(0, true);
    if (!n || n > 268435456 || p + 8 + n > bytes.length) {
      truncated = true;
      break;
    }
    const payload = bytes.subarray(p + 8, p + 8 + n);
    if (crc(payload) !== v.getUint32(4, true)) {
      truncated = true;
      break;
    }
    if (payload[0] === 0 && base === null) base = payload.slice(1);
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
  const candidates = [];
  for (const name of ["journal-a.bin", "journal-b.bin"])
    try {
      const f = await directory.getFileHandle(name);
      const bytes = new Uint8Array(await (await f.getFile()).arrayBuffer());
      const parsed = parse(bytes);
      if (parsed) candidates.push({ name, file: f, ...parsed });
    } catch {}
  candidates.sort((a, b) => b.generation - a.generation);
  if (candidates.length) {
    active = candidates[0];
    generation = active.generation;
    offset = active.validLength;
    if (active.truncated) {
      const h = await active.file.createSyncAccessHandle();
      try {
        h.truncate(offset);
        h.flush();
      } finally {
        h.close();
      }
    }
  }
}
async function checkpoint(bytes) {
  await init();
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
    const r = record(0, bytes);
    h.write(r, { at: 12 });
    h.truncate(12 + r.length);
    h.flush();
    generation++;
    offset = 12 + r.length;
    active = {
      name,
      file,
      generation,
      base: bytes,
      records: [],
      validLength: offset,
      truncated: false,
    };
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
    };
  else if (data.t === "load") {
    value = active
      ? {
          base: active.base,
          records: active.records,
          truncated: active.truncated,
        }
      : null;
  } else if (data.t === "checkpoint") {
    await checkpoint(new Uint8Array(data.bytes));
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
  }
  if (data.t === "ping") value = true;
  return value;
}
