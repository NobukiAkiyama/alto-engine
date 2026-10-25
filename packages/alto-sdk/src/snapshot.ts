import type {
  AltoWarning,
  DocNode,
  DocumentAsset,
  DocumentMeta,
  NodeId,
  TileRef,
  VersionStatus,
} from "./types.js";

/** The document as it is now. Frozen; nodes that did not change keep their identity. */
export interface DocumentSnapshot {
  readonly revision: number;
  readonly meta: DocumentMeta;
  readonly root: NodeId;
  readonly nodes: ReadonlyMap<NodeId, DocNode>;
  readonly selection: TileRef | null;
  readonly assets: readonly DocumentAsset[];
  readonly history: { readonly undo: number; readonly redo: number };
  readonly versions: VersionStatus;
  /** True when the document changed since it was last saved. */
  readonly dirty: boolean;
  /** Identifies the content and the version head; changes with every edit. */
  readonly stateHash: string;
  readonly warnings: readonly AltoWarning[];
}

/** What one update changed, so a host redraws only that. */
export interface DocumentChange {
  readonly revision: number;
  readonly nodes: { readonly added: NodeId[]; readonly updated: NodeId[]; readonly removed: NodeId[] };
  readonly meta: boolean;
  readonly selection: boolean;
  readonly assets: boolean;
  readonly history: boolean;
  readonly versions: boolean;
  readonly dirty: boolean;
}

/** @internal A node as the Worker sends it: content under `kind`. */
export interface WireNode {
  id: NodeId;
  parent: NodeId | null;
  name: string;
  props: DocNode["props"];
  kind: { kind?: string; [field: string]: unknown };
}
/** @internal Fields every state reply carries. */
interface WireEnvelope {
  revision: number;
  history: { undo: number; redo: number };
  versions: VersionStatus;
  dirty: boolean;
  stateHash: string;
  warnings: AltoWarning[];
}
/** @internal A full view (open, create, save) or a patch (an edit). */
export type WireState =
  | (WireEnvelope & {
      patch?: false;
      document: {
        meta: DocumentMeta;
        root: NodeId;
        nodes: Record<NodeId, WireNode>;
        selection: TileRef | null;
        assets?: Record<string, DocumentAsset>;
      };
    })
  | (WireEnvelope & {
      patch: true;
      nodes?: Record<NodeId, WireNode>;
      removed?: NodeId[];
      meta?: DocumentMeta;
      selection?: TileRef | null;
      assets?: Record<string, DocumentAsset>;
    });

function deepFreeze<T>(value: T): T {
  if (value && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const v of Object.values(value)) deepFreeze(v);
  }
  return value;
}

const KNOWN = new Set(["group", "raster", "text", "vector", "adjustment"]);

/** @internal */
export function toNode(wire: WireNode): DocNode {
  const base = { id: wire.id, parent: wire.parent, name: wire.name, props: wire.props };
  const k = wire.kind;
  let node: DocNode;
  if (!k.kind || !KNOWN.has(k.kind)) node = { ...base, type: "unsupported", raw: k };
  else if (k.kind === "group")
    node = {
      ...base,
      type: "group",
      children: k.children as NodeId[],
      isolation: k.isolation as "pass_through" | "isolated",
    };
  else if (k.kind === "raster") node = { ...base, type: "raster", tiles: k.tiles as never };
  else if (k.kind === "text") node = { ...base, type: "text", content: k.content as never };
  else if (k.kind === "vector") node = { ...base, type: "vector", paths: k.paths as never };
  else node = { ...base, type: "adjustment", adjustment: k.adjustment as never };
  return deepFreeze(node);
}

/** @internal Applies Worker states and keeps the current snapshot. */
export class SnapshotStore {
  private current: DocumentSnapshot | null = null;
  /** Each node's wire form as last applied: a full view reuses nodes whose form is unchanged. */
  private wire = new Map<NodeId, string>();
  private node(id: NodeId, wire: WireNode, previous: DocNode | undefined): DocNode {
    const json = JSON.stringify(wire);
    if (previous && this.wire.get(id) === json) return previous;
    this.wire.set(id, json);
    return toNode(wire);
  }
  get(): DocumentSnapshot {
    if (!this.current) throw new Error("no document state yet");
    return this.current;
  }
  /** Null when `state` is a patch this store already holds: the Worker sends each edit's
   * state twice, as a notice and as the reply. */
  apply(state: WireState): DocumentChange | null {
    const before = this.current;
    if (state.patch && before && state.revision === before.revision && state.stateHash === before.stateHash)
      return null;
    const nodes = new Map(before?.nodes);
    const added: NodeId[] = [];
    const updated: NodeId[] = [];
    const removed: NodeId[] = [];
    let meta: DocumentMeta, root: NodeId, selection: TileRef | null, assets: readonly DocumentAsset[];
    if (state.patch) {
      if (!before) throw new Error("patch before a full view");
      for (const id of state.removed ?? []) if (nodes.delete(id)) removed.push(id), this.wire.delete(id);
      for (const [id, wire] of Object.entries(state.nodes ?? {})) {
        (nodes.has(id) ? updated : added).push(id);
        nodes.set(id, this.node(id, wire, undefined));
      }
      meta = state.meta ?? before.meta;
      root = before.root;
      selection = "selection" in state ? (state.selection ?? null) : before.selection;
      assets = state.assets ? Object.values(state.assets) : before.assets;
    } else {
      const doc = state.document;
      const next = new Map<NodeId, DocNode>();
      for (const [id, wire] of Object.entries(doc.nodes)) {
        const previous = nodes.get(id);
        const node = this.node(id, wire, previous);
        next.set(id, node);
        if (!previous) added.push(id);
        else if (node !== previous) updated.push(id);
      }
      for (const id of nodes.keys()) if (!next.has(id)) removed.push(id), this.wire.delete(id);
      nodes.clear();
      for (const [id, n] of next) nodes.set(id, n);
      meta = doc.meta;
      root = doc.root;
      selection = doc.selection;
      assets = Object.values(doc.assets ?? {});
    }
    const snapshot: DocumentSnapshot = deepFreeze({
      revision: state.revision,
      meta,
      root,
      nodes,
      selection,
      assets,
      history: state.history,
      versions: state.versions,
      dirty: state.dirty,
      stateHash: state.stateHash,
      warnings: state.warnings ?? [],
    });
    // A Map is not frozen by Object.freeze; expose it read-only by type and keep it private.
    this.current = snapshot;
    const same = <T>(a: T | undefined, b: T) => JSON.stringify(a) === JSON.stringify(b);
    return {
      revision: snapshot.revision,
      nodes: { added, updated, removed },
      meta: !before || !same(before.meta, meta),
      selection: !before || !same(before.selection, selection),
      assets: !before || before.assets !== assets,
      history: !before || !same(before.history, snapshot.history),
      versions: !before || !same(before.versions, snapshot.versions),
      dirty: !before || before.dirty !== snapshot.dirty,
    };
  }
}
