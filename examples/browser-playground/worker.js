import init, { Spike } from './pkg/alto_wasm.js';

let engine;
let busy = false;
let pendingView;
let lastView = { x: 0, y: 0, zoom: 0.5 };

self.onmessage = async ({ data }) => {
  try {
    if (data.type === 'init') {
      await init(new URL('./pkg/alto_wasm_bg.wasm', import.meta.url));
      engine = await Spike.create(data.canvas);
      postMessage({ type: 'ready' });
    } else if (data.type === 'resize' && engine) {
      engine.resize(data.width, data.height);
    } else if (data.type === 'render' && engine) {
      lastView = data;
      pendingView = data;
      if (!busy) {
        busy = true;
        while (pendingView) {
          const view = pendingView;
          pendingView = null;
          engine.render(view.x, view.y, view.zoom);
        }
        busy = false;
      }
    } else if (data.type === 'readback' && engine) {
      const ms = await engine.rewriteAndReadTile(data.index);
      engine.render(lastView.x, lastView.y, lastView.zoom);
      postMessage({ type: 'readback', ms });
    } else if (data.type === 'undo' && engine) {
      const changed = engine.undo();
      engine.render(lastView.x, lastView.y, lastView.zoom);
      postMessage({ type: 'history', message: changed ? '文書のUndoを適用しました' : '戻せる編集はありません', canUndo: engine.canUndo(), canRedo: engine.canRedo() });
    } else if (data.type === 'redo' && engine) {
      const changed = engine.redo();
      engine.render(lastView.x, lastView.y, lastView.zoom);
      postMessage({ type: 'history', message: changed ? '文書のRedoを適用しました' : 'やり直せる編集はありません', canUndo: engine.canUndo(), canRedo: engine.canRedo() });
    }
  } catch (error) {
    postMessage({ type: 'error', message: String(error) });
  }
};
