import init, { compareVectorBackends, verifyVectorOptimization } from './pkg/alto_wasm.js';
self.onmessage = async ({ data }) => {
  try {
    await init();
    const report = data?.mode === 'optimization'
      ? await verifyVectorOptimization(data.frames ?? 100, !!data.reverse)
      : await compareVectorBackends();
    postMessage(JSON.parse(report));
  } catch (error) { postMessage({ error: String(error.stack || error) }); }
};
