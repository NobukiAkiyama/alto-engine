import { testing } from "../../packages/alto-sdk/dist/testing.js";
import { unstable } from "../../packages/alto-sdk/dist/unstable.js";

/** Exercise actual device-loss recovery even when the page has no animation frames. */
export async function gpuRestored(engine, action = () => testing(engine).destroyGpu()) {
  const gpu = unstable(engine);
  let restored = false, finished = false, timer, off;
  const recovery = new Promise((resolve, reject) => {
    off = gpu.onGpu((event) => {
      if (event === "restored") {
        restored = true;
        resolve();
      }
    });
    timer = setTimeout(() => reject(Error("GPU recovery timeout")), 30000);
  });
  const driveRecovery = async () => {
    await action();
    while (!restored && !finished) {
      try {
        await gpu.settle();
      } catch (error) {
        if (error.code !== "GPU_LOST") throw error;
      }
      if (!restored && !finished) await new Promise((resolve) => setTimeout(resolve, 100));
    }
  };
  try {
    await Promise.race([recovery, driveRecovery()]);
  } finally {
    finished = true;
    clearTimeout(timer);
    off();
  }
}
