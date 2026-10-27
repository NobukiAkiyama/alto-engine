// The engine inside a React app: React owns the layout, the engine owns its canvas,
// and the layer list follows the document through useSyncExternalStore.
import { createEngine, type DocumentHandle, type Engine, type NodeId } from "@alto/engine";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { createRoot } from "react-dom/client";

function useSnapshot(doc: DocumentHandle) {
  return useSyncExternalStore(
    (onChange) => doc.subscribe(onChange),
    () => doc.getSnapshot(),
  );
}

function Layers({ doc, selected, onSelect }: { doc: DocumentHandle; selected: NodeId; onSelect(id: NodeId): void }) {
  const { nodes, root, history } = useSnapshot(doc);
  const top = nodes.get(root);
  const children = top?.type === "group" ? [...top.children].reverse() : [];
  return (
    <aside>
      <button onClick={() => doc.transact("layer.add", (tx) => onSelect(tx.addRaster({ name: `Layer ${children.length + 1}` })))}>
        Add layer
      </button>
      <button disabled={!history.undo} onClick={() => doc.undo()}>
        Undo
      </button>
      <ul>
        {children.map((id) => (
          <li key={id} className={id === selected ? "active" : ""} onClick={() => onSelect(id)}>
            {nodes.get(id)?.name}
          </li>
        ))}
      </ul>
    </aside>
  );
}

function Editor() {
  const container = useRef<HTMLDivElement>(null);
  const [engine, setEngine] = useState<Engine | null>(null);
  const [doc, setDoc] = useState<DocumentHandle | null>(null);
  const [displayPhase,setDisplayPhase]=useState("preparing");
  useEffect(()=>engine?.on("presentation",p=>setDisplayPhase(p.phase)),[engine]);
  const [layer, setLayer] = useState<NodeId>("");
  useEffect(() => {
    let disposed = false;
    let started: Engine | null = null;
    void (async () => {
      started = await createEngine({ container: container.current!, storage: { namespace: "react-embed" } });
      if (disposed) return void started.dispose();
      const d = started.recovery.available
        ? await started.recovery.restore()
        : await started.createDocument({ width: 1000, height: 700 });
      const id = await d.transact("layer.add", (tx) => tx.addRaster({ name: "Layer 1" }));
      setEngine(started);
      setDoc(d);
      setLayer(id);
    })();
    return () => {
      disposed = true;
      void started?.dispose();
    };
  }, []);
  useEffect(() => {
    if (engine && doc) Object.assign(window, { altoExample: { engine, document: () => doc, layer: () => layer } });
  }, [engine, doc, layer]);
  const draw = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!doc || !engine?.presentation.interactionReady) return;
    const el = event.currentTarget;
    el.setPointerCapture(event.pointerId);
    const stroke = doc.tools.begin("brush", { layer, color: [0.8, 0.1, 0.2, 1], tip: { size: 18 } });
    stroke.input(event.nativeEvent);
    const move = (e: PointerEvent) => stroke.input(e);
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", () => (el.removeEventListener("pointermove", move), void stroke.end()), { once: true });
  };
  return (
    <>
      <div className="editor" ref={container} onPointerDown={draw} />
      <output>{displayPhase === "ready" ? "" : "Updating view…"}</output>
      {doc ? <Layers doc={doc} selected={layer} onSelect={setLayer} /> : <aside>Starting…</aside>}
    </>
  );
}

createRoot(document.getElementById("app")!).render(<Editor />);
