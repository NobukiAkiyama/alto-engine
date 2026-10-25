function deepFreeze(value) {
    if (value && typeof value === "object" && !Object.isFrozen(value)) {
        Object.freeze(value);
        for (const v of Object.values(value))
            deepFreeze(v);
    }
    return value;
}
const KNOWN = new Set(["group", "raster", "text", "vector", "adjustment"]);
/** @internal */
export function toNode(wire) {
    const base = { id: wire.id, parent: wire.parent, name: wire.name, props: wire.props };
    const k = wire.kind;
    let node;
    if (!k.kind || !KNOWN.has(k.kind))
        node = { ...base, type: "unsupported", raw: k };
    else if (k.kind === "group")
        node = {
            ...base,
            type: "group",
            children: k.children,
            isolation: k.isolation,
        };
    else if (k.kind === "raster")
        node = { ...base, type: "raster", tiles: k.tiles };
    else if (k.kind === "text")
        node = { ...base, type: "text", content: k.content };
    else if (k.kind === "vector")
        node = { ...base, type: "vector", paths: k.paths };
    else
        node = { ...base, type: "adjustment", adjustment: k.adjustment };
    return deepFreeze(node);
}
/** @internal Applies Worker states and keeps the current snapshot. */
export class SnapshotStore {
    current = null;
    /** Each node's wire form as last applied: a full view reuses nodes whose form is unchanged. */
    wire = new Map();
    node(id, wire, previous) {
        const json = JSON.stringify(wire);
        if (previous && this.wire.get(id) === json)
            return previous;
        this.wire.set(id, json);
        return toNode(wire);
    }
    get() {
        if (!this.current)
            throw new Error("no document state yet");
        return this.current;
    }
    apply(state) {
        const before = this.current;
        const nodes = new Map(before?.nodes);
        const added = [];
        const updated = [];
        const removed = [];
        let meta, root, selection, assets;
        if (state.patch) {
            if (!before)
                throw new Error("patch before a full view");
            for (const id of state.removed ?? [])
                if (nodes.delete(id))
                    removed.push(id), this.wire.delete(id);
            for (const [id, wire] of Object.entries(state.nodes ?? {})) {
                (nodes.has(id) ? updated : added).push(id);
                nodes.set(id, this.node(id, wire, undefined));
            }
            meta = state.meta ?? before.meta;
            root = before.root;
            selection = "selection" in state ? (state.selection ?? null) : before.selection;
            assets = state.assets ? Object.values(state.assets) : before.assets;
        }
        else {
            const doc = state.document;
            const next = new Map();
            for (const [id, wire] of Object.entries(doc.nodes)) {
                const previous = nodes.get(id);
                const node = this.node(id, wire, previous);
                next.set(id, node);
                if (!previous)
                    added.push(id);
                else if (node !== previous)
                    updated.push(id);
            }
            for (const id of nodes.keys())
                if (!next.has(id))
                    removed.push(id), this.wire.delete(id);
            nodes.clear();
            for (const [id, n] of next)
                nodes.set(id, n);
            meta = doc.meta;
            root = doc.root;
            selection = doc.selection;
            assets = Object.values(doc.assets ?? {});
        }
        const snapshot = deepFreeze({
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
        const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
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
