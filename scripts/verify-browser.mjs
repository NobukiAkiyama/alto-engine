// Run the shipped Playground suites in a fresh headless Chrome profile.
// --functional omits the hardware-dependent B1-B7 benchmark for CI.
// ALTO_BROWSER_GPU=swiftshader runs WebGPU on a software adapter in CI.
// --examples opens the Vite builds of examples/minimal-embed and react-embed instead
// and drives each through the public API it exposes (build them first).
// --suite=functional (or comma-separated names) narrows a diagnostic run.
// Every run saves Chrome stderr and CDP GPU/process/console diagnostics. An explicit
// ALTO_BROWSER_EXPECTED_VERSION refuses a changed browser; watchdogs stay on unless
// ALTO_BROWSER_DISABLE_GPU_WATCHDOG=1 is set for a controlled comparison.
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, createReadStream, existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { platform, tmpdir } from "node:os";
import { extname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("../", import.meta.url)));
const functionalOnly = process.argv.includes("--functional");
const benchmarkOnly = process.argv.includes("--benchmark-only");
const examplesOnly = process.argv.includes("--examples");
const lifecycleOnly = process.argv.includes("--gpu-lifecycle");
const selectedSuites = process.argv.find((arg) => arg.startsWith("--suite="))?.slice(8).split(",");
const disableGpuWatchdog = process.env.ALTO_BROWSER_DISABLE_GPU_WATCHDOG === "1";
const expectedVersion = process.env.ALTO_BROWSER_EXPECTED_VERSION;
const browserDpr = Number(process.env.ALTO_BROWSER_DPR || 0);
const browserWidth = Number(process.env.ALTO_BROWSER_WIDTH || 0);
const browserHeight = Number(process.env.ALTO_BROWSER_HEIGHT || 0);
const benchmarkView = process.env.ALTO_BROWSER_VIEW_MODE || "physical";
if (benchmarkOnly && functionalOnly) throw Error("Choose --benchmark-only or --functional");
if (benchmarkView !== "physical" && benchmarkView !== "css")
  throw Error("ALTO_BROWSER_VIEW_MODE must be physical or css");
if (browserDpr && (!Number.isFinite(browserDpr) || browserDpr < 0.5 || browserDpr > 3))
  throw Error("ALTO_BROWSER_DPR must be between 0.5 and 3");
if ((browserWidth && !browserHeight) || (!browserWidth && browserHeight))
  throw Error("Set both ALTO_BROWSER_WIDTH and ALTO_BROWSER_HEIGHT");
const reportDir = process.env.ALTO_BROWSER_REPORT_DIR || "verification";
const externalReports = !!process.env.ALTO_BROWSER_REPORT_DIR;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const mime = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json",
  ".wasm": "application/wasm",
  ".png": "image/png",
  ".otf": "font/otf",
  ".ttf": "font/ttf",
  ".alto": "application/zip",
  ".css": "text/css; charset=utf-8",
};

/** Run in each built example. It exposes `window.altoExample` from the SDK it bundled. */
const exampleCheck = `(async () => {
  const assert = (ok, m) => { if (!ok) throw Error(m); };
  const { engine, document: current, layer } = window.altoExample;
  const doc = current();
  const before = await doc.snapshotHash();
  const stroke = doc.tools.begin("brush", { layer: layer(), color: [1, 0, 0, 1], tip: { size: 40 } });
  for (let i = 0; i < 10; i++) stroke.input({ x: 100 + i * 20, y: 100, pressure: 1 });
  await stroke.end();
  const drawn = await doc.snapshotHash();
  assert(drawn !== before, "the stroke did not change the document");
  await doc.undo();
  assert((await doc.snapshotHash()) === before, "undo did not restore the document");
  await doc.redo();
  assert((await doc.snapshotHash()) === drawn, "redo did not restore the stroke");
  const png = await doc.exportImage();
  const image = await createImageBitmap(png);
  const { width, height } = doc.getSnapshot().meta;
  assert(png.type === "image/png" && image.width === width && image.height === height, "PNG export " + image.width + "x" + image.height);
  let archive;
  await doc.save({ write: async (stream) => { archive = await new Response(stream).blob(); } });
  assert(!doc.getSnapshot().dirty, "save left the document dirty");
  const reopened = await engine.open(archive);
  assert((await reopened.snapshotHash()) === drawn, "the reopened document differs");
  return { pass: true, worker: engine.status.state };
})()`;

function chromeBin() {
  if (process.env.CHROME) return process.env.CHROME;
  const candidates =
    platform() === "darwin"
      ? ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"]
      : platform() === "win32"
        ? ["C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"]
        : ["/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium"];
  const found = candidates.find(existsSync);
  if (!found) throw Error("Chrome not found; set CHROME to its executable path");
  return found;
}

async function startServer() {
  const server = createServer(async (req, res) => {
    try {
      const pathname = decodeURIComponent(new URL(req.url, "http://127.0.0.1").pathname);
      let path = resolve(root, "." + pathname);
      if (path !== root && !path.startsWith(root + sep)) {
        res.writeHead(403).end();
        return;
      }
      if ((await stat(path)).isDirectory()) path = join(path, "index.html");
      res.setHeader("Content-Type", mime[extname(path)] || "application/octet-stream");
      createReadStream(path).on("error", () => res.destroy()).pipe(res);
    } catch {
      res.writeHead(404).end();
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  return server;
}

async function browserPage(profile, proc, chromeError) {
  for (let i = 0; i < 160; i++) {
    if (chromeError()) throw Error("Chrome failed to start: " + chromeError());
    if (proc.exitCode !== null) throw Error("Chrome exited with " + proc.exitCode);
    try {
      const port = Number((await readFile(join(profile, "DevToolsActivePort"), "utf8")).split("\n")[0]);
      const version = await fetch(`http://127.0.0.1:${port}/json/version`).then((r) => r.json());
      const pages = await fetch(`http://127.0.0.1:${port}/json/list`).then((r) => r.json());
      const page = pages.find((p) => p.type === "page");
      if (page) return { version, page };
    } catch {}
    await sleep(250);
  }
  throw Error("Chrome debugging port did not open");
}

async function connect(url, onEvent = () => {}) {
  const ws = new WebSocket(url);
  const pending = new Map();
  let id = 0;
  ws.addEventListener("message", ({ data }) => {
    const message = JSON.parse(data);
    if (message.method) onEvent(message);
    const request = pending.get(message.id);
    if (!request) return;
    clearTimeout(request.timer);
    pending.delete(message.id);
    if (message.error) request.reject(Error(JSON.stringify(message.error)));
    else request.resolve(message);
  });
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(Error("Chrome WebSocket did not open")), 15000);
    ws.addEventListener("open", () => { clearTimeout(timer); resolve(); }, { once: true });
    ws.addEventListener("error", () => { clearTimeout(timer); reject(Error("Chrome WebSocket failed")); }, { once: true });
  });
  return {
    close: () => ws.close(),
    send: (method, params = {}, timeoutMs = 600000) => new Promise((resolve, reject) => {
      const requestId = ++id;
      const timer = setTimeout(() => {
        pending.delete(requestId);
        reject(Error("CDP timeout: " + method));
      }, timeoutMs);
      pending.set(requestId, { resolve, reject, timer });
      ws.send(JSON.stringify({ id: requestId, method, params }));
    }),
  };
}

const suites = [
  ["functional", `const { runTests } = await import('../../tests/e2e/browser-tests.js'); await runTests(window.altoEngine, false, (line) => console.log(line));`],
  ["m5", `const { runM5Tests } = await import('../../tests/e2e/m5-tests.js'); await runM5Tests(window.altoEngine, (line) => console.log(line));`],
  ["m6", `const { runM6Tests } = await import('../../tests/e2e/m6-tests.js'); await runM6Tests(window.altoEngine, (line) => console.log(line));`],
  ["m7", `const { runM7Tests } = await import('../../tests/e2e/m7-tests.js'); await runM7Tests(window.altoEngine, (line) => console.log(line));`],
  ["m8", `const { runM8Tests } = await import('../../tests/e2e/m8-tests.js'); await runM8Tests(window.altoEngine, (line) => console.log(line));`],
  ["embed", `const { runEmbedTests } = await import('../../tests/e2e/embed-tests.js'); await runEmbedTests(window.altoEngine, (line) => console.log(line));`],
  ["benchmark", `const { runTests } = await import('../../tests/e2e/browser-tests.js'); await runTests(window.altoEngine, true, (line) => console.log(line));`],
];
if (selectedSuites?.some((name) => !suites.some(([suite]) => suite === name)))
  throw Error("Unknown --suite: " + selectedSuites.join(","));

let server, chrome, client, browserClient, profile;
const summary = [];
const diagnostics = { startedAt: new Date().toISOString(), expectedVersion: expectedVersion ?? null, watchdogDisabled: disableGpuWatchdog, browser: null, executable: null, args: [], events: [], system: [] };
let stderr = "";
const recordEvent = ({ method, params }) => {
  if (!["Log.entryAdded", "Runtime.exceptionThrown", "Runtime.consoleAPICalled", "Target.targetCrashed"].includes(method)) return;
  diagnostics.events.push({ at: new Date().toISOString(), method, params });
  if (diagnostics.events.length > 500) diagnostics.events.shift();
};
async function sampleSystem(label) {
  if (!browserClient) return;
  const sample = { label, at: new Date().toISOString() };
  for (const method of ["SystemInfo.getInfo", "SystemInfo.getProcessInfo"])
    try { sample[method] = (await browserClient.send(method, {}, 5000)).result; }
    catch (error) { sample[method] = { error: String(error) }; }
  diagnostics.system.push(sample);
  await writeFile(join(reportDir, "browser-diagnostics.json"), JSON.stringify(diagnostics, null, 2) + "\n");
}
try {
  await mkdir(reportDir, { recursive: true });
  await writeFile(join(reportDir, "chrome-stderr.log"), "");
  diagnostics.artifacts = {};
  for (const file of ["packages/alto-sdk/dist/worker.js", "packages/alto-sdk/dist/pkg/alto_wasm_bg.wasm"])
    diagnostics.artifacts[file] = createHash("sha256").update(await readFile(join(root, file))).digest("hex");
  server = await startServer();
  const address = server.address();
  const query = new URLSearchParams();
  if (benchmarkView === "css") query.set("benchmarkView", "css");
  if (process.env.ALTO_BROWSER_PROFILE === "1") query.set("profile", "1");
  const pageUrl = `http://127.0.0.1:${address.port}/${lifecycleOnly ? "tests/diagnostics/gpu-lifecycle.html" : "examples/browser-playground/"}${query.size ? "?" + query : ""}`;
  profile = await mkdtemp(join(tmpdir(), "alto-verify-chrome-"));
  let launchError = null;
  const executable = chromeBin();
  const args = [
    "--remote-debugging-port=0",
    `--user-data-dir=${profile}`,
    "--headless=new",
    "--enable-unsafe-webgpu",
    "--enable-logging=stderr",
    ...(disableGpuWatchdog ? ["--disable-gpu-watchdog"] : []),
    ...(process.env.ALTO_BROWSER_GPU === "swiftshader"
      ? ["--use-webgpu-adapter=swiftshader", "--enable-dawn-features=allow_unsafe_apis", "--enable-unsafe-swiftshader"]
      : platform() === "darwin" ? ["--use-angle=metal"] : []),
    "--no-first-run",
    "--disable-gpu-sandbox",
    "--window-size=1280,800",
    "about:blank",
  ];
  diagnostics.executable = executable;
  diagnostics.args = args;
  chrome = spawn(executable, args, { stdio: ["ignore", "ignore", "pipe"] });
  chrome.on("error", (error) => { launchError = error; });
  chrome.stderr.on("data", (chunk) => {
    stderr = (stderr + chunk).slice(-2 * 1024 * 1024);
    // Keep the evidence even if a hung test is killed before finally runs.
    appendFileSync(join(reportDir, "chrome-stderr.log"), chunk);
  });
  const { version, page } = await browserPage(profile, chrome, () => launchError);
  console.log(version.Browser);
  diagnostics.browser = version;
  if (expectedVersion && version.Browser.split("/").pop() !== expectedVersion)
    throw Error(`Expected Chrome ${expectedVersion}, got ${version.Browser}`);
  browserClient = await connect(version.webSocketDebuggerUrl, recordEvent);
  await browserClient.send("Target.setDiscoverTargets", { discover: true }, 5000);
  client = await connect(page.webSocketDebuggerUrl, recordEvent);
  const { send } = client;
  const evaluate = async (expression, timeoutMs = 600000) => {
    const message = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, timeoutMs);
    const details = message.result?.exceptionDetails;
    if (details) throw Error(details.text + " " + (details.exception?.description || ""));
    return message.result?.result?.value;
  };
  await send("Page.enable");
  await send("Runtime.enable");
  await send("Log.enable");
  await sampleSystem("startup");
  if (browserDpr || browserWidth) {
    await send("Emulation.setDeviceMetricsOverride", {
      width: browserWidth || 1280,
      height: browserHeight || 800,
      deviceScaleFactor: browserDpr || 1,
      mobile: false,
    });
  }
  if (lifecycleOnly) {
    await send("Page.navigate", { url: pageUrl }, 30000);
    let ready = false;
    for (let i = 0; i < 120; i++) {
      ready = !!(await evaluate("!!window.runGpuLifecycle").catch(() => false));
      if (ready) break;
      await sleep(100);
    }
    if (!ready) throw Error("Raw WebGPU fixture did not load");
    const options = { mode: process.env.ALTO_GPU_LIFECYCLE_MODE || "stable", cycles: Number(process.env.ALTO_GPU_LIFECYCLE_CYCLES || 120) };
    if (!Number.isInteger(options.cycles) || options.cycles < 1 || options.cycles > 2000) throw Error("invalid lifecycle cycles");
    const result = await evaluate(`window.runGpuLifecycle(${JSON.stringify(options)})`, 180000);
    await writeFile(join(reportDir, "gpu-lifecycle.json"), JSON.stringify(result, null, 2) + "\n");
    summary.push({ name: "gpu-lifecycle " + options.mode, ok: result.pass, count: result.cycles ?? result.cyclesCompleted, error: result.error });
    await sampleSystem("gpu-lifecycle " + options.mode);
  } else if (examplesOnly) {
    for (const name of ["minimal-embed", "react-embed"]) {
      console.log("RUN example", name);
      const url = `http://127.0.0.1:${address.port}/examples/${name}/dist/index.html`;
      const nav = await send("Page.navigate", { url });
      if (nav.result?.errorText) throw Error(nav.result.errorText);
      let started = false;
      for (let i = 0; i < 240 && !started; i++) {
        started = !!(await evaluate("!!window.altoExample?.document()").catch(() => false));
        if (!started) await sleep(250);
      }
      let error = started ? null : "the example did not start";
      if (started) {
        try {
          await evaluate(exampleCheck);
        } catch (e) {
          error = e.message;
        }
      }
      console.log(name, error ? "FAIL " + error : "PASS");
      summary.push({ name: "example " + name, ok: !error, count: 1, error });
      await sampleSystem("example " + name);
    }
  } else {
    const navigation = await send("Page.navigate", { url: pageUrl });
    if (navigation.result?.errorText) throw Error(navigation.result.errorText);
    let ready = false;
    for (let i = 0; i < 240; i++) {
      ready = !!(await evaluate("!!window.altoEngine && !!navigator.gpu").catch(() => false));
      if (ready) break;
      await sleep(250);
    }
    if (!ready) {
      const error = await evaluate("document.querySelector('#error')?.textContent || ''").catch(() => "");
      throw Error("Engine did not start: " + error + "\n" + stderr);
    }
    const adapter = await evaluate(`(async () => {
      const info = (await navigator.gpu.requestAdapter())?.info;
      return info ? { vendor: info.vendor, architecture: info.architecture, device: info.device, description: info.description } : null;
    })()`);
    diagnostics.adapter = adapter;
    console.log("WebGPU adapter", JSON.stringify(adapter));
    if (process.env.ALTO_BROWSER_GPU === "swiftshader" && !adapter?.architecture?.includes("swiftshader"))
      throw Error("SwiftShader was requested but a different WebGPU adapter was selected");
    await evaluate(`if (!document.querySelector('#recovery').hidden) document.querySelector('#discard').click()`);
    for (let i = 0; i < 120; i++) {
      if (await evaluate("!!window.altoEngine.document")) break;
      await sleep(250);
      if (i === 119) throw Error("Document did not open");
    }

    for (const [name, body] of suites) {
      if (selectedSuites && !selectedSuites.includes(name)) continue;
      if (functionalOnly && name === "benchmark") continue;
      if (benchmarkOnly && name !== "benchmark") continue;
      console.log("RUN", name);
      await evaluate("document.querySelector('#test-results').dataset.report = ''");
      let suiteError = null;
      try {
        await evaluate(`(async () => { ${body} })()`);
      } catch (error) {
        suiteError = error;
      }
      const report = await evaluate("document.querySelector('#test-results').dataset.report || ''");
      if (report) {
        const file = externalReports ? join(reportDir, `${name}.json`) : join(reportDir, `rebuild-${name}.json`);
        await writeFile(file, report.endsWith("\n") ? report : report + "\n");
      }
      const parsed = report ? JSON.parse(report) : null;
      const results = parsed?.results || [];
      const failed = results.filter((result) => result.pass === false);
      const ok = !suiteError && results.length > 0 && failed.length === 0;
      console.log(name, "results", results.length, "failed", failed.length);
      summary.push({ name, ok, count: results.length, error: suiteError?.message });
      await sampleSystem(name);
      if (!ok) break;
    }
  }
} catch (error) {
  console.error("Browser verification failed:", error);
  summary.push({ name: "startup", ok: false, error: String(error) });
} finally {
  await sampleSystem("shutdown");
  await writeFile(join(reportDir, "browser-diagnostics.json"), JSON.stringify(diagnostics, null, 2) + "\n");
  if (externalReports) await writeFile(join(reportDir, "summary.json"), JSON.stringify(summary, null, 2) + "\n");
  if (summary.some((suite) => !suite.ok))
    console.error("Chrome stderr (tail):\n" + stderr.slice(-12000));
  client?.close();
  browserClient?.close();
  if (chrome && chrome.exitCode === null && chrome.signalCode === null) {
    chrome.kill();
    await new Promise((resolve) => {
      chrome.once("exit", resolve);
      setTimeout(resolve, 5000);
    });
  }
  if (server) await new Promise((resolve) => server.close(resolve));
  if (profile) await rm(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
}
console.log(JSON.stringify(summary, null, 2));
if (!summary.length || summary.some((suite) => !suite.ok)) process.exitCode = 1;
