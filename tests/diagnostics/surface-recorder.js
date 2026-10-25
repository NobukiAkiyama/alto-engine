// Loaded only by the diagnostic HTTP server; it does not modify shipped assets.
(() => {
  const ids = new WeakMap(), events = [];
  let serial = 0, submits = 0, acquisitions = 0;
  const realmId = crypto.randomUUID(), failedDevices = new Set(), destroyedDevices = new Set();
  const id = (object) => {
    if (!object) return null;
    if (!ids.has(object)) ids.set(object, ++serial);
    return ids.get(object);
  };
  const dimensions = (canvas) => canvas ? [canvas.width, canvas.height] : null;
  const event = (op, fields = {}) => {
    const entry = { at: Date.now(), time: performance.now(), realm: location.href, realmId, op, ...fields };
    events.push(entry);
    if (events.length > 1024) events.shift();
    if (op !== "submit" && op !== "acquire") console.log("SURFACE", JSON.stringify(entry));
  };
  const dump = (cause, device) => {
    if (failedDevices.has(device)) return;
    failedDevices.add(device);
    console.log("SURFACE-FAILURE", JSON.stringify({ cause, events }));
  };
  const method = (prototype, name, wrap) => {
    const original = prototype?.[name];
    if (original) prototype[name] = wrap(original);
  };
  method(globalThis.GPUAdapter?.prototype, "requestDevice", (original) => function (...args) {
    event("request-device", { adapter: id(this) });
    return original.apply(this, args).then((device) => {
      const deviceId = id(device);
      event("device-ready", { adapter: id(this), device: deviceId, queue: id(device.queue) });
      device.addEventListener("uncapturederror", ({ error }) => {
        event("validation", { device: deviceId, message: error.message });
        if (!destroyedDevices.has(deviceId)) dump(error.message, deviceId);
      });
      device.lost.then(({ reason, message }) => { event("device-lost", { device: deviceId, reason, message }); if (!destroyedDevices.has(deviceId)) dump(message, deviceId); });
      return device;
    });
  });
  method(globalThis.GPUDevice?.prototype, "destroy", (original) => function (...args) {
    destroyedDevices.add(id(this));
    event("destroy-device", { device: id(this) });
    return original.apply(this, args);
  });
  method(globalThis.GPUCanvasContext?.prototype, "configure", (original) => function (config) {
    event("configure", { context: id(this), canvas: id(this.canvas), dimensions: dimensions(this.canvas), device: id(config.device), format: config.format, usage: config.usage, alphaMode: config.alphaMode });
    return original.call(this, config);
  });
  method(globalThis.GPUCanvasContext?.prototype, "unconfigure", (original) => function () {
    event("unconfigure", { context: id(this), canvas: id(this.canvas) });
    return original.call(this);
  });
  method(globalThis.GPUCanvasContext?.prototype, "getCurrentTexture", (original) => function () {
    const texture = original.call(this);
    event("acquire", { context: id(this), canvas: id(this.canvas), dimensions: dimensions(this.canvas), texture: id(texture), width: texture.width, height: texture.height, index: ++acquisitions });
    return texture;
  });
  method(globalThis.GPUQueue?.prototype, "submit", (original) => function (...args) {
    event("submit", { queue: id(this), index: ++submits });
    return original.apply(this, args);
  });
  for (const type of [globalThis.OffscreenCanvas, globalThis.HTMLCanvasElement]) {
    if (!type) continue;
    for (const key of ["width", "height"]) {
      const descriptor = Object.getOwnPropertyDescriptor(type.prototype, key);
      if (!descriptor?.set || !descriptor.get) continue;
      Object.defineProperty(type.prototype, key, { ...descriptor, set(value) {
        event("resize-" + key, { canvas: id(this), from: descriptor.get.call(this), to: value, dimensions: dimensions(this) });
        return descriptor.set.call(this, value);
      } });
    }
  }
  method(globalThis.Worker?.prototype, "terminate", (original) => function () {
    event("terminate-worker", { worker: id(this) });
    return original.call(this);
  });
  event("recorder-ready");
})();
