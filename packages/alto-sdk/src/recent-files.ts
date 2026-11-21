import { AltoError } from "./errors.js";

const DATABASE = "alto-engine-recent";
const FILES = "files";
const MAX_RECENT_FILES = 32;

/** A file or project folder saved in this browser's recent list. */
export interface RecentFile {
  /** Stable identifier for this entry. */
  id: string;
  /** The display name of the file or folder. */
  name: string;
  /** Whether this entry opens as an archive or a project folder. */
  kind: "file" | "folder";
  /** The browser handle used to reopen the entry. */
  handle: FileSystemFileHandle | FileSystemDirectoryHandle;
  /** The object store for a thin archive, when supplied. */
  history?: FileSystemDirectoryHandle;
  /** When the entry was last remembered, in milliseconds since the Unix epoch. */
  opened: number;
}

function openDatabase(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const unavailable = (cause?: unknown) =>
      new AltoError("UNSUPPORTED", "recent files require IndexedDB", undefined, { cause });
    let request: IDBOpenDBRequest;
    try {
      request = globalThis.indexedDB.open(DATABASE, 1);
    } catch (cause) {
      reject(unavailable(cause));
      return;
    }
    request.onupgradeneeded = () => request.result.createObjectStore(FILES, { keyPath: "id" });
    request.onerror = () => reject(unavailable(request.error));
    request.onsuccess = () => {
      const db = request.result;
      db.onversionchange = () => db.close();
      resolve(db);
    };
  });
}

type OnResult = <T>(request: IDBRequest<T>, use: (result: T) => void) => void;

async function access<T>(
  mode: IDBTransactionMode,
  run: (store: IDBObjectStore, done: (value: T) => void, onResult: OnResult) => void,
): Promise<T> {
  const db = await openDatabase();
  try {
    return await new Promise<T>((resolve, reject) => {
      const tx = db.transaction(FILES, mode);
      let result: T;
      tx.oncomplete = () => resolve(result);
      tx.onabort = () => reject(tx.error);
      const guarded = (work: () => void) => {
        try {
          work();
        } catch (cause) {
          reject(cause);
          tx.abort();
        }
      };
      // Follow-up writes are queued in the request event while the transaction is active.
      const onResult: OnResult = (request, use) => {
        request.onsuccess = () => guarded(() => use(request.result));
      };
      guarded(() => run(tx.objectStore(FILES), (value) => { result = value; }, onResult));
    });
  } catch (cause) {
    throw new AltoError("STORAGE", "could not access recent files", undefined, { cause });
  } finally {
    db.close();
  }
}

type RecentHandle = RecentFile["handle"];
type StoredHandle = RecentHandle | { opfsPath: string[] };
type StoredFile = Omit<RecentFile, "handle" | "history"> & {
  handle: StoredHandle;
  history?: FileSystemDirectoryHandle | { opfsPath: string[] };
};

async function opfsLocation(handle: RecentHandle) {
  const root = await navigator.storage.getDirectory();
  const opfsPath = await root.resolve(handle);
  if (!opfsPath) throw new AltoError("UNSUPPORTED", "this browser cannot persist external file handles");
  return { opfsPath };
}

async function reopenHandle(handle: StoredHandle, kind: RecentFile["kind"]): Promise<RecentHandle> {
  if (!("opfsPath" in handle)) return handle;
  const path = handle.opfsPath;
  let directory = await navigator.storage.getDirectory();
  for (const name of path.slice(0, -1)) directory = await directory.getDirectoryHandle(name);
  const name = path.at(-1);
  if (kind === "folder") return name === undefined ? directory : directory.getDirectoryHandle(name);
  if (name === undefined) throw new AltoError("CORRUPT_DATA", "recent file has no OPFS filename");
  return directory.getFileHandle(name);
}

async function reopenEntry(entry: StoredFile): Promise<RecentFile> {
  const { handle, history, ...metadata } = entry;
  return {
    ...metadata,
    handle: await reopenHandle(handle, entry.kind),
    ...(history ? { history: await reopenHandle(history, "folder") as FileSystemDirectoryHandle } : {}),
  };
}

async function openAvailable(entry: StoredFile): Promise<RecentFile | undefined> {
  try {
    return await reopenEntry(entry);
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === "NotFoundError") return undefined;
    throw cause;
  }
}

function entries(): Promise<StoredFile[]> {
  return access("readonly", (store, done, onResult) => onResult(store.getAll(), done));
}

const newestFirst = (a: Pick<RecentFile, "opened">, b: Pick<RecentFile, "opened">) => b.opened - a.opened;

/**
 * Saves a file or folder in the recent list and returns its stable ID.
 * Omit `history` to retain its previous value, or pass `null` to remove it.
 */
export async function rememberFile(
  handle: FileSystemFileHandle | FileSystemDirectoryHandle,
  options?: { history?: FileSystemDirectoryHandle | null },
): Promise<RecentFile> {
  const kind = handle.kind === "directory" ? "folder" : "file";
  const newHistory = options?.history;
  if (newHistory && kind === "folder")
    throw new AltoError("INVALID_ARGUMENT", "a separate history folder applies to archives");
  let storedHandle: StoredHandle = handle;
  let storedHistory: StoredFile["history"] | null = newHistory;
  for (;;) {
    const before = await entries();
    let matched: string | undefined;
    try {
      for (const entry of before) {
        let existing: RecentHandle;
        try {
          existing = await reopenHandle(entry.handle, entry.kind);
        } catch (cause) {
          if (cause instanceof DOMException && cause.name === "NotFoundError") continue;
          throw cause;
        }
        if (await handle.isSameEntry(existing)) {
          matched = entry.id;
          break;
        }
      }
    } catch (cause) {
      throw new AltoError("STORAGE", "could not compare recent file handles", undefined, { cause });
    }

    // isSameEntry runs outside IndexedDB. Recheck the IDs in the write transaction
    // so a concurrent insert or deletion can neither create duplicates nor be lost.
    const save = () => access<StoredFile | undefined>("readwrite", (store, done, onResult) => {
      onResult<StoredFile[]>(store.getAll(), (current) => {
        if (current.length !== before.length || current.some((entry, i) => entry.id !== before[i].id)) return;
        const previous = current.find((entry) => entry.id === matched);
        const latest = current.reduce((time, entry) => Math.max(time, entry.opened), 0);
        const entry: StoredFile = {
          id: previous?.id ?? crypto.randomUUID(),
          name: handle.name,
          kind,
          handle: storedHandle,
          opened: Math.max(Date.now(), latest + 1),
        };
        const history = storedHistory === undefined ? previous?.history : storedHistory;
        if (history) entry.history = history;
        const retained = current.filter((item) => item.id !== entry.id).sort(newestFirst);
        for (const old of retained.slice(MAX_RECENT_FILES - 1)) store.delete(old.id);
        store.put(entry);
        done(entry);
      });
    });
    try {
      const saved = await save();
      if (saved) return reopenEntry(saved);
    } catch (error) {
      if (!(error instanceof AltoError && error.cause instanceof DOMException &&
        error.cause.name === "DataCloneError") || "opfsPath" in storedHandle) throw error;
      // Some browsers provide OPFS but cannot store its native handles in IndexedDB.
      storedHandle = await opfsLocation(handle);
      if (newHistory) storedHistory = await opfsLocation(newHistory);
    }
  }
}

/** Lists remembered files newest first without checking permissions. */
export async function recentFiles(): Promise<RecentFile[]> {
  try {
    const available = await Promise.all((await entries()).map(openAvailable));
    return available.filter((entry) => entry !== undefined).sort(newestFirst);
  } catch (error) {
    if (error instanceof AltoError && error.code === "UNSUPPORTED") return [];
    throw error;
  }
}

/** Removes an entry from this browser's recent list. */
export async function forgetFile(id: string): Promise<void> {
  try {
    await access<void>("readwrite", (store, done, onResult) => onResult(store.delete(id), done));
  } catch (error) {
    if (!(error instanceof AltoError && error.code === "UNSUPPORTED")) throw error;
  }
}

type PermissionHandle = FileSystemHandle & {
  queryPermission?(options: { mode: "readwrite" }): Promise<PermissionState>;
  requestPermission?(options: { mode: "readwrite" }): Promise<PermissionState>;
};

async function requirePermissions(handles: PermissionHandle[]): Promise<void> {
  const options = { mode: "readwrite" } as const;
  let granted: boolean[];
  try {
    // Browsers can provide OPFS handles without implementing the picker permission API.
    const states = await Promise.all(handles.map(async (handle) => {
      if (!handle.queryPermission && !handle.requestPermission) return "granted";
      return handle.queryPermission?.(options);
    }));
    // Start both prompts within the same user gesture when a thin archive and
    // its history folder need permission.
    granted = await Promise.all(handles.map(async (handle, i) =>
      states[i] === "granted" || await handle.requestPermission?.(options) === "granted"));
  } catch (cause) {
    throw new AltoError("STORAGE", "permission denied", undefined, { cause });
  }
  if (granted.some((value) => !value)) throw new AltoError("STORAGE", "permission denied");
}

export async function permittedRecentFile(id: string): Promise<RecentFile> {
  const row = await access<StoredFile | undefined>("readonly", (store, done, onResult) => onResult(store.get(id), done));
  const entry = row && await openAvailable(row);
  if (!entry) throw new AltoError("NOT_FOUND", "recent file not found");
  await requirePermissions(entry.history ? [entry.handle, entry.history] : [entry.handle]);
  return entry;
}
