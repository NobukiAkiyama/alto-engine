import { AltoError } from "./errors.js";
function nodeId() {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    let timestamp = BigInt(Date.now());
    for (let i = 5; i >= 0; i--) {
        bytes[i] = Number(timestamp & 255n);
        timestamp >>= 8n;
    }
    bytes[6] = (bytes[6] & 15) | 112;
    bytes[8] = (bytes[8] & 63) | 128;
    const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
const IDENTITY = [1, 0, 0, 1, 0, 0];
const props = () => ({
    visible: true,
    locked: false,
    opacity: 1,
    blend: "normal",
    transform: IDENTITY,
    mask: null,
    mask_enabled: true,
    mask_linked: true,
    mask_transform: IDENTITY,
    vector_mask: null,
    clip_path: null,
    clip_to_below: false,
    effects: [],
});
/**
 * The operations of one edit. Build them in the `transact` callback, which must be
 * synchronous; the edit is applied as a whole and undoes as one step.
 */
export class Transaction {
    snapshot;
    /** @internal */
    ops = [];
    /** @internal */
    constructor(snapshot) {
        this.snapshot = snapshot;
    }
    addRaster(options = {}) {
        return this.insert({ kind: "raster", tiles: { format: "rgba8", tiles: {} } }, options, "Layer");
    }
    addGroup(options = {}) {
        const isolation = options.isolated ? "isolated" : "pass_through";
        return this.insert({ kind: "group", children: [], isolation }, options, "Group");
    }
    /** A raster layer showing an imported image. */
    addRasterFromAsset(asset, options = {}) {
        const image = asset;
        const id = this.insert({ kind: "raster", tiles: image.tiles }, options, "Image");
        this.ops.push({ type: "set_asset", id: image.record.id, record: image.record });
        return id;
    }
    /** Registers an imported font in the document; text can use it afterwards. Returns its asset id. */
    addFont(font) {
        const record = font.record;
        this.ops.push({ type: "set_asset", id: record.id, record });
        return record.id;
    }
    addText(content, options = {}) {
        const name = options.name || content.text.split("\n")[0].slice(0, 24) || "Text";
        return this.insert({ kind: "text", content }, { ...options, name }, "Text");
    }
    setText(id, content) {
        this.ops.push({ type: "set_text", id, content });
    }
    addVector(paths = [], options = {}) {
        return this.insert({ kind: "vector", paths }, options, "Vector");
    }
    setVector(id, paths) {
        this.ops.push({ type: "set_vector", id, paths });
    }
    setVectorMask(id, mask) {
        this.ops.push({ type: "set_vector_mask", id, mask });
    }
    setClipPath(id, path) {
        this.ops.push({ type: "set_clip_path", id, path });
    }
    /** An adjustment layer applying `adjustment` to the layers below it. */
    addAdjustment(adjustment, options = {}) {
        return this.insert({ kind: "adjustment", adjustment }, options, "Adjustment");
    }
    setAdjustment(id, adjustment) {
        this.ops.push({ type: "set_adjustment", id, adjustment });
    }
    /** Replaces a layer's pixel mask (R8 tiles), or removes it with null. */
    setMask(id, mask) {
        this.ops.push({ type: "set_mask", layer: id, mask });
    }
    removeNode(id) {
        this.ops.push({ type: "remove_node", id });
    }
    moveNode(id, parent, index) {
        this.ops.push({ type: "move_node", id, parent, index });
    }
    setProps(id, patch) {
        this.ops.push({
            type: "set_props",
            id,
            patch: {
                name: null,
                visible: null,
                locked: null,
                opacity: null,
                blend: null,
                transform: null,
                clip_to_below: null,
                effects: null,
                ...patch,
            },
        });
    }
    /** Sets the transform from a matrix or from position, scale and rotation (radians). */
    setTransform(id, t) {
        const m = Array.isArray(t)
            ? t
            : [
                Math.cos(t.rotation) * t.scaleX,
                Math.sin(t.rotation) * t.scaleX,
                -Math.sin(t.rotation) * t.scaleY,
                Math.cos(t.rotation) * t.scaleY,
                t.x,
                t.y,
            ];
        this.setProps(id, { transform: m });
    }
    insert(kind, options, fallback) {
        const id = nodeId();
        const parent = options.parent || this.snapshot.root;
        const created = this.ops.some((o) => o.type === "insert_node" && o.node.id === parent);
        let count = 0;
        if (!created) {
            const node = this.snapshot.nodes.get(parent);
            if (!node)
                throw new AltoError("NOT_FOUND", `no node ${parent}`, { node: parent });
            count = node.type === "group" ? node.children.length : 0;
        }
        const index = options.index ?? count + this.ops.filter((o) => o.type === "insert_node" && o.parent === parent).length;
        this.ops.push({
            type: "insert_node",
            node: { id, parent: null, name: options.name || fallback, props: props(), kind },
            parent,
            index,
        });
        return id;
    }
}
