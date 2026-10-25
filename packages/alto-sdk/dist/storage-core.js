// Two alternating, checksummed OPFS journals. A checkpoint becomes visible after flush.
// Format 4 checkpoints keep objects once each in pack files under packs/, and write only
// the objects the store lacks, into one new pack per checkpoint; the journal records
// where each object is. Format 3 kept one file per object under objects/, which made
// recovery open thousands of files. Earlier formats hold a whole package. All are still
// read, so the next checkpoint migrates them.
const MAGIC = new TextEncoder().encode("ALTOJ001");
const MAX_RECORD_BYTES = 540 * 1024 * 1024;
const OBJECT_PATH = /^objects\/([0-9a-f]{2})\/([0-9a-f]{64})$/;
const PACK_NAME = /^[0-9]+\.pack$/;
let namespace = "default", directory = null, objectsDirectory, packsDirectory, active = null, candidates = [], generation = 0, offset = 0, 
// Where each stored object is: { pack, offset, length }, or { loose: true } for a
// format 3 file. Nothing is deleted mid-session: a checkpoint skips objects listed
// here, so one must never vanish, and every location recorded stays readable.
stored = new Map(), 
// What a valid checkpoint referenced at startup; the rest is garbage.
keptPacks = new Set(), keptLoose = new Set(), 
// Parsed checkpoints and records stay in memory only until recovery is decided.
released = false, 
// Unreferenced objects are deleted once per session, before its first write.
collected = false;
const encoder = new TextEncoder();
/** A directory's entries; the WebWorker lib does not declare the async iterator. */
const entries = (dir) => dir.entries();
const decoder = new TextDecoder();
async function sha256(bytes) {
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}
const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let i = 0; i < 8; i++)
        c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
});
function crc(bytes) {
    let c = 0xffffffff;
    for (const b of bytes)
        c = crcTable[(c ^ b) & 255] ^ (c >>> 8);
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
    for (const v of entries.values())
        size += 8 + names[i++].length + v.length;
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
    if (bytes.length < 12 || !MAGIC.every((b, i) => bytes[i] === b))
        return null;
    let p = 12, base = null, metadata = null, records = [], truncated = false;
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
        if (payload[0] === 0 && base === null)
            base = payload.slice(1);
        else if (payload[0] >= 2 && payload[0] <= 4 && base === null) {
            if (payload.length < 5) {
                truncated = true;
                break;
            }
            const metaLength = new DataView(payload.buffer, payload.byteOffset + 1, 4).getUint32(0, true);
            if (metaLength > 4 * 1024 * 1024 || 5 + metaLength >= payload.length) {
                truncated = true;
                break;
            }
            try {
                metadata = JSON.parse(decoder.decode(payload.subarray(5, 5 + metaLength)));
            }
            catch {
                truncated = true;
                break;
            }
            base = payload.slice(5 + metaLength);
        }
        else if (payload[0] === 1 && base !== null)
            records.push(payload.slice(1));
        else {
            truncated = true;
            break;
        }
        p += 8 + n;
    }
    return base
        ? {
            generation: new DataView(bytes.buffer, bytes.byteOffset + 8, 4).getUint32(0, true),
            base,
            metadata,
            records,
            truncated,
            validLength: p,
        }
        : null;
}
const isHash = (hash) => typeof hash === "string" && /^[0-9a-f]{64}$/.test(hash);
async function validMetadata(candidate) {
    const meta = candidate.metadata;
    if (!meta)
        return true;
    if (!Array.isArray(meta.opaque) || meta.opaque.some((hash) => !isHash(hash)))
        return false;
    if (meta.format === 2)
        return isHash(meta.packageSha256) && (await sha256(candidate.base)) === meta.packageSha256;
    if (meta.format !== 3 && meta.format !== 4)
        return false;
    const objects = meta.objects;
    if (!isHash(meta.entriesSha256) ||
        !Array.isArray(objects) ||
        !objects.every((path) => typeof path === "string" && OBJECT_PATH.test(path)))
        return false;
    if (meta.format === 4) {
        const packs = meta.packs;
        const valid = Array.isArray(packs) &&
            packs.every((name) => typeof name === "string" && PACK_NAME.test(name)) &&
            Array.isArray(meta.locations) &&
            meta.locations.length === objects.length &&
            meta.locations.every((l) => Array.isArray(l) &&
                l.length === 3 &&
                l.every(Number.isSafeInteger) &&
                l[0] >= 0 &&
                l[0] < packs.length &&
                l[1] >= 0 &&
                l[2] > 0);
        if (!valid)
            return false;
    }
    return (await sha256(candidate.base)) === meta.entriesSha256;
}
async function listObjects() {
    const present = new Map();
    for await (const [shard, dir] of entries(objectsDirectory)) {
        if (dir.kind !== "directory")
            continue;
        for await (const [name] of entries(dir)) {
            const path = `objects/${shard}/${name}`;
            if (OBJECT_PATH.test(path))
                present.set(path, dir);
        }
    }
    return present;
}
/** The folder name of a namespace: its UTF-8 bytes in hex, so any allowed name is a valid name. */
export function namespaceFolder(name) {
    return "ns-" + Array.from(encoder.encode(name), (b) => b.toString(16).padStart(2, "0")).join("");
}
async function init() {
    if (directory)
        return;
    const root = await (await navigator.storage.getDirectory()).getDirectoryHandle("alto-engine", { create: true });
    directory = await root.getDirectoryHandle(namespaceFolder(namespace), { create: true });
    objectsDirectory = await directory.getDirectoryHandle("objects", { create: true });
    packsDirectory = await directory.getDirectoryHandle("packs", { create: true });
    await scan();
}
/**
 * Reads both journals and keeps the valid checkpoints. Objects are not listed here:
 * that is slow with thousands of them, and startup waits for this. A missing object
 * fails its checkpoint when recovery reads it.
 */
async function scan() {
    const found = [];
    const seenGenerations = [];
    for (const name of ["journal-a.bin", "journal-b.bin"])
        try {
            const f = await directory.getFileHandle(name);
            const bytes = new Uint8Array(await (await f.getFile()).arrayBuffer());
            if (bytes.length >= 12 && MAGIC.every((b, i) => bytes[i] === b))
                seenGenerations.push(new DataView(bytes.buffer, bytes.byteOffset + 8, 4).getUint32(0, true));
            const parsed = parse(bytes);
            if (parsed)
                found.push({ name, file: f, recordCount: 0, ...parsed });
        }
        catch { }
    generation = Math.max(0, ...seenGenerations);
    candidates = [];
    for (const candidate of found)
        if (await validMetadata(candidate)) {
            candidate.recordCount = candidate.records.length;
            candidates.push(candidate);
        }
    candidates.sort((a, b) => b.generation - a.generation);
    // Newest first, so an object a later checkpoint copied is read from its new pack. A
    // rescan keeps what this session wrote: those files are still there.
    for (const { metadata: meta } of candidates) {
        if (meta?.format === 3)
            for (const path of meta.objects) {
                keptLoose.add(path);
                if (!stored.has(path))
                    stored.set(path, { loose: true });
            }
        if (meta?.format === 4) {
            const packs = meta.packs;
            for (const name of packs)
                keptPacks.add(name);
            meta.objects.forEach((path, i) => {
                const [pack, offset, length] = meta.locations[i];
                if (!stored.has(path))
                    stored.set(path, { pack: packs[pack], offset, length });
            });
        }
    }
    released = false;
    if (candidates.length) {
        active = candidates[0];
        offset = active.validLength;
    }
}
/**
 * Deletes packs and object files no valid checkpoint named at startup: written for a
 * checkpoint that never completed (possibly partial), or only by a generation since
 * replaced. Runs before the session's first write, so nothing it deletes was written
 * or listed to the engine in this session.
 */
async function collectGarbage() {
    if (collected)
        return;
    collected = true;
    for await (const [name, handle] of entries(packsDirectory))
        if (handle.kind === "file" && !keptPacks.has(name))
            await packsDirectory.removeEntry(name).catch(() => { });
    for (const [path, dir] of await listObjects())
        if (!keptLoose.has(path))
            await dir.removeEntry(path.split("/")[2]).catch(() => { });
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
        if (h.write(bytes, { at: 0 }) !== bytes.length)
            throw Error("Short object write");
        h.flush();
    }
    finally {
        h.close();
    }
}
/** Whole packs, read once each and in parallel. */
async function readPacks(names) {
    const unique = [...new Set(names)];
    const bytes = await Promise.all(unique.map(async (name) => {
        const file = await (await packsDirectory.getFileHandle(name)).getFile();
        return new Uint8Array(await file.arrayBuffer());
    }));
    return new Map(unique.map((name, i) => [name, bytes[i]]));
}
function slicePack(packs, { pack, offset, length }) {
    const bytes = packs.get(pack);
    if (offset + length > bytes.length)
        throw Error(`pack ${pack} is shorter than its objects`);
    return bytes.subarray(offset, offset + length);
}
/** Format 3 objects, one OPFS read each; overlapping them hides most of the latency. */
async function readLoose(paths) {
    const objects = new Array(paths.length);
    let next = 0;
    const reader = async () => {
        while (next < paths.length) {
            const i = next++;
            objects[i] = await readObject(paths[i]);
        }
    };
    await Promise.all(Array.from({ length: Math.min(32, paths.length) }, reader));
    return objects;
}
/** The package a candidate recovers from: formats 3 and 4 rejoin entries and objects. */
async function packageOf(candidate) {
    const meta = candidate.metadata;
    if (meta?.format !== 3 && meta?.format !== 4)
        return candidate.base;
    const framed = unframe(candidate.base);
    const paths = meta.objects;
    if (meta.format === 3) {
        const objects = await readLoose(paths);
        paths.forEach((path, i) => framed.set(path, objects[i]));
    }
    else {
        const names = meta.packs;
        const packs = await readPacks(names);
        paths.forEach((path, i) => {
            const [pack, offset, length] = meta.locations[i];
            framed.set(path, slicePack(packs, { pack: names[pack], offset, length }));
        });
    }
    return frame(framed);
}
async function writeGeneration(tag, payload, metadata) {
    const name = active?.name === "journal-a.bin" ? "journal-b.bin" : "journal-a.bin";
    const file = await directory.getFileHandle(name, { create: true });
    const h = await file.createSyncAccessHandle();
    try {
        const header = new Uint8Array(12);
        header.set(MAGIC);
        new DataView(header.buffer).setUint32(8, generation + 1, true);
        h.truncate(0);
        h.write(header, { at: 0 });
        const r = record(tag, payload);
        if (h.write(r, { at: 12 }) !== r.length)
            throw Error("Short checkpoint write");
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
    }
    finally {
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
 * Which stored objects the new pack also takes, so that recovery reads few packs and
 * little dead data. Packs are visited newest first. One whose live objects come to less
 * than twice what the new pack already holds is merged into it; packs then grow
 * geometrically with age, and a store has about log2(total / one checkpoint) of them.
 * A pack mostly of objects no checkpoint references any more is copied too, and so are
 * format 3 files.
 */
async function objectsToCopy(paths, newBytes) {
    const loose = [];
    const live = new Map();
    for (const path of paths) {
        const at = stored.get(path);
        if (at.loose)
            loose.push(path);
        else {
            const pack = live.get(at.pack) || { bytes: 0, paths: [] };
            pack.bytes += at.length;
            pack.paths.push(path);
            live.set(at.pack, pack);
        }
    }
    const looseBytes = await readLoose(loose);
    const copied = new Map(loose.map((path, i) => [path, looseBytes[i]]));
    let pending = newBytes + looseBytes.reduce((n, b) => n + b.length, 0);
    const merge = [];
    for (const name of [...live.keys()].sort((a, b) => parseInt(b) - parseInt(a))) {
        const pack = live.get(name);
        const size = (await (await packsDirectory.getFileHandle(name)).getFile()).size;
        if (pack.bytes < 2 * pending || 2 * pack.bytes < size) {
            merge.push(name);
            pending += pack.bytes;
        }
    }
    const packs = await readPacks(merge);
    for (const name of merge)
        for (const path of live.get(name).paths)
            copied.set(path, slicePack(packs, stored.get(path)));
    return copied;
}
/** Writes and flushes one pack; returns where each object went. */
async function writePack(name, objects) {
    const file = await packsDirectory.getFileHandle(name, { create: true });
    const h = await file.createSyncAccessHandle();
    const locations = new Map();
    try {
        h.truncate(0);
        let at = 0;
        for (const [path, bytes] of objects) {
            if (h.write(bytes, { at }) !== bytes.length)
                throw Error("Short pack write");
            locations.set(path, { pack: name, offset: at, length: bytes.length });
            at += bytes.length;
        }
        h.flush();
    }
    finally {
        h.close();
    }
    return locations;
}
/**
 * `framed` is the engine's checkpoint. Objects missing from the store are written and
 * flushed, in a new pack, before the journal that names them. With `loose`, every
 * object it names is written as its own file instead (format 3), so tests can produce
 * a journal the previous SDK wrote.
 */
async function checkpoint(framed, loose = false) {
    await init();
    await collectGarbage();
    const entries = unframe(framed);
    const meta = JSON.parse(decoder.decode(entries.get("checkpoint.json")));
    entries.delete("checkpoint.json");
    const added = new Map();
    for (const [path, bytes] of entries) {
        if (!OBJECT_PATH.test(path))
            continue;
        if (!stored.has(path))
            added.set(path, bytes);
        entries.delete(path);
    }
    const missing = meta.objects.filter((path) => !stored.has(path) && !added.has(path));
    if (missing.length)
        throw Error("checkpoint names objects the store lacks: " + missing.slice(0, 3).join(", "));
    const base = frame(entries);
    const metadata = {
        format: loose ? 3 : 4,
        entriesSha256: await sha256(base),
        objects: meta.objects,
        opaque: meta.opaque,
    };
    let moved;
    if (loose) {
        const old = meta.objects.filter((path) => stored.get(path) && !stored.get(path).loose);
        const packs = await readPacks(old.map((path) => stored.get(path).pack));
        for (const path of old)
            added.set(path, slicePack(packs, stored.get(path)));
        for (const [path, bytes] of added)
            await writeObject(path, bytes);
        moved = new Map([...added.keys()].map((path) => [path, { loose: true }]));
    }
    else {
        let newBytes = 0;
        for (const bytes of added.values())
            newBytes += bytes.length;
        const copied = await objectsToCopy(meta.objects.filter((path) => stored.has(path)), newBytes);
        for (const [path, bytes] of copied)
            added.set(path, bytes);
        const written = added.size ? await writePack(`${generation + 1}.pack`, added) : new Map();
        moved = written;
        const at = (path) => (written.get(path) || stored.get(path));
        const packs = [...new Set(meta.objects.map((path) => at(path).pack))];
        const index = new Map(packs.map((name, i) => [name, i]));
        metadata.packs = packs;
        metadata.locations = meta.objects.map((path) => {
            const { pack, offset, length } = at(path);
            return [index.get(pack), offset, length];
        });
    }
    await writeGeneration(metadata.format, withMetadata(metadata, base), metadata);
    // Only now: a failed write leaves the store as the engine was told it is.
    for (const [path, location] of moved)
        stored.set(path, location);
    release();
}
/** Format 1, kept so tests can produce a journal an older engine wrote. */
async function checkpointLegacy(bytes) {
    await init();
    await writeGeneration(0, bytes, null);
    release();
}
async function damageCheckpoint(candidate, preserveRecordCrc) {
    if (!candidate)
        throw Error("Journal has no checkpoint");
    const bytes = new Uint8Array(await (await candidate.file.getFile()).arrayBuffer());
    const recordLength = new DataView(bytes.buffer, 12, 4).getUint32(0, true);
    const metaLength = candidate.metadata
        ? new DataView(bytes.buffer, 21, 4).getUint32(0, true)
        : 0;
    const objectOffset = candidate.metadata ? 25 + metaLength : 21;
    if (objectOffset >= 20 + recordLength)
        throw Error("Checkpoint has no package");
    bytes[objectOffset] ^= 1;
    if (preserveRecordCrc)
        new DataView(bytes.buffer).setUint32(16, crc(bytes.subarray(20, 20 + recordLength)), true);
    const h = await candidate.file.createSyncAccessHandle();
    try {
        if (preserveRecordCrc)
            h.write(bytes.subarray(16, 20), { at: 16 });
        h.write(bytes.subarray(objectOffset, objectOffset + 1), { at: objectOffset });
        h.flush();
    }
    finally {
        h.close();
    }
}
/** Handles one journal request; shared by the Storage Worker and the inline fallback. */
/** Deletes this namespace's journals and objects and starts an empty store. */
async function discard() {
    await init();
    const root = await (await navigator.storage.getDirectory()).getDirectoryHandle("alto-engine");
    await root.removeEntry(namespaceFolder(namespace), { recursive: true });
    directory = null;
    active = null;
    candidates = [];
    offset = 0;
    stored = new Map();
    keptPacks = new Set();
    keptLoose = new Set();
    released = false;
    collected = true;
    await init();
}
export async function handle(data) {
    // The session pings before the Worker names the namespace; opening the store here
    // would open the default one.
    if (data.t === "ping")
        return true;
    if (data.t === "configure") {
        if (directory && data.namespace !== namespace)
            throw Error("CONFLICT: the store is already open for another namespace");
        namespace = data.namespace;
        await init();
        return true;
    }
    if (data.t === "discard") {
        await discard();
        return true;
    }
    await init();
    let value;
    if (data.t === "inspect")
        value = {
            available: !!active?.recordCount,
            records: active?.recordCount || 0,
            truncated: !!active?.truncated,
            latestGeneration: generation,
            objects: [...stored.keys()],
            packs: active?.metadata?.packs?.length || 0,
        };
    else if (data.t === "loadCandidates") {
        // Newest first, without their contents: recovery usually needs only the first.
        if (released)
            await scan();
        value = candidates.map((candidate) => ({
            generation: candidate.generation,
            latestGeneration: generation,
            truncated: candidate.truncated,
        }));
    }
    else if (data.t === "loadCandidate") {
        if (released)
            await scan();
        const candidate = candidates.find((c) => c.generation === data.generation);
        if (!candidate)
            throw Error("Journal generation unavailable");
        value = {
            base: await packageOf(candidate),
            metadata: candidate.metadata && { format: candidate.metadata.format, opaque: candidate.metadata.opaque },
            records: candidate.records,
        };
    }
    else if (data.t === "activate") {
        const selected = candidates.find((candidate) => candidate.generation === data.generation);
        if (!selected)
            throw Error("Journal generation unavailable");
        active = selected;
        offset = active.validLength;
        if (active.truncated) {
            const h = await active.file.createSyncAccessHandle();
            try {
                h.truncate(offset);
                h.flush();
            }
            finally {
                h.close();
            }
            active.truncated = false;
        }
        release();
        value = true;
    }
    else if (data.t === "checkpoint") {
        await checkpoint(new Uint8Array(data.entries));
        value = true;
    }
    else if (data.t === "checkpointLoose") {
        await checkpoint(new Uint8Array(data.entries), true);
        value = true;
    }
    else if (data.t === "checkpointLegacy") {
        await checkpointLegacy(new Uint8Array(data.bytes));
        value = true;
    }
    else if (data.t === "append") {
        if (!active)
            throw Error("Journal has no checkpoint");
        await collectGarbage();
        const bytes = new Uint8Array(data.bytes);
        const r = record(1, bytes);
        const h = await active.file.createSyncAccessHandle();
        try {
            const written = h.write(r, { at: offset });
            if (written !== r.length)
                throw Error("Short journal write");
            h.flush();
        }
        finally {
            h.close();
        }
        offset += r.length;
        active.recordCount++;
        value = true;
    }
    else if (data.t === "corruptTail") {
        if (!active)
            throw Error("Journal has no checkpoint");
        const h = await active.file.createSyncAccessHandle();
        try {
            h.write(new Uint8Array([10, 0, 0]), { at: offset });
            h.flush();
        }
        finally {
            h.close();
        }
        value = true;
    }
    else if (data.t === "corruptCheckpoint" || data.t === "corruptCheckpointCrc") {
        await damageCheckpoint(active, data.t === "corruptCheckpoint");
        value = true;
    }
    else if (data.t === "corruptBothCheckpoints") {
        for (const candidate of candidates)
            await damageCheckpoint(candidate, true);
        value = true;
    }
    if (data.t === "ping")
        value = true;
    return value;
}
