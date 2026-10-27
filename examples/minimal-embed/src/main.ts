// The smallest editor around the engine: one brush, undo, image export and .alto files.
import { createEngine, isSupported, type AltoError, type DocumentHandle, type NodeId } from "@alto/engine";
import { downloadTarget, fileSource } from "@alto/engine/browser";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const status = (text: string) => ($("status").textContent = text);

const support = await isSupported();
if (!support.ok) throw new Error("This browser cannot run the engine: " + support.reason);

const editor = $("editor");
const engine = await createEngine({ container: editor, storage: { namespace: "minimal-embed" } });
engine.on("status", ({ state, error }) => status(error ? `${state}: ${error.code}` : state));
engine.on("presentation", ({phase}) => { if(phase!=="ready") status("Updating view…"); });
engine.on("error", (e: AltoError) => status("error: " + e.code));

let doc: DocumentHandle;
let layer: NodeId;
async function start(d: DocumentHandle) {
  doc = d;
  const { nodes, root } = doc.getSnapshot();
  // A raster layer to paint on: the topmost one, or a new one.
  const top = nodes.get(root);
  const children = top?.type === "group" ? top.children : [];
  const raster = [...children].reverse().find((id) => nodes.get(id)?.type === "raster");
  layer = raster ?? (await doc.transact("layer.add", (tx) => tx.addRaster({ name: "Paint" })));
  doc.subscribe(() => {
    const { history, dirty } = doc.getSnapshot();
    $<HTMLButtonElement>("undo").disabled = !history.undo;
    $<HTMLButtonElement>("redo").disabled = !history.redo;
    status(dirty ? "edited" : "saved");
  });
}
await start(engine.recovery.available ? await engine.recovery.restore() : await engine.createDocument({ width: 1200, height: 800 }));

// Strokes: pointer events go to the container, which outlives the engine's canvas.
editor.addEventListener("pointerdown", (event) => {
  if (!engine.presentation.interactionReady) return;
  editor.setPointerCapture(event.pointerId);
  const stroke = doc.tools.begin("brush", { layer, color: [0.1, 0.3, 0.9, 1], tip: { size: 16 } });
  stroke.input(event);
  const move = (e: PointerEvent) => stroke.input(e);
  editor.addEventListener("pointermove", move);
  editor.addEventListener(
    "pointerup",
    () => {
      editor.removeEventListener("pointermove", move);
      void stroke.end();
    },
    { once: true },
  );
});
editor.addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    engine.viewport.zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY * 0.001));
  },
  { passive: false },
);

$("new").onclick = async () => start(await engine.createDocument({ width: 1200, height: 800 }));
$("undo").onclick = () => doc.undo();
$("redo").onclick = () => doc.redo();
$("png").onclick = async () => {
  const png = await doc.exportImage();
  const a = Object.assign(document.createElement("a"), { href: URL.createObjectURL(png), download: "drawing.png" });
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
};
$("save").onclick = () => doc.save(downloadTarget("drawing.alto"));
$("open").onclick = () => $("file").click();
$<HTMLInputElement>("file").onchange = async (e) => {
  const file = (e.target as HTMLInputElement).files?.[0];
  if (file) await start(await engine.open(fileSource(file)));
};

// For scripts/verify-browser.mjs --examples, which drives this page.
Object.assign(window, { altoExample: { engine, document: () => doc, layer: () => layer } });
