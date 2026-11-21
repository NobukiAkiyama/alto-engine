// Run the in-page correctness suites in native Safari or Firefox via WebDriver.
// GPU process diagnostics and the CDP-based crash suite remain in verify-browser.mjs.
import { createReadStream } from "node:fs";
import { mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("../", import.meta.url)));
const browser = process.argv.find((arg) => arg.startsWith("--browser="))?.slice(10);
if (!["safari", "firefox"].includes(browser)) throw Error("Use --browser=safari or --browser=firefox");
const driverUrl = process.env.ALTO_WEBDRIVER_URL || "http://127.0.0.1:4444";
const selected = process.argv.find((arg) => arg.startsWith("--suite="))?.slice(8).split(",") ||
  ["functional", "image-versions", "m5", "m6", "m7", "m8", "embed", "parity"];
const allSuites = new Set(["functional", "image-versions", "m5", "m6", "m7", "m8", "embed", "parity"]);
if (selected.some((name) => !allSuites.has(name))) throw Error("Unsupported suite: " + selected.join(","));
const reportDir = process.env.ALTO_BROWSER_REPORT_DIR || "verification/out";
const mime = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".json": "application/json", ".wasm": "application/wasm", ".png": "image/png", ".otf": "font/otf", ".ttf": "font/ttf", ".alto": "application/zip", ".css": "text/css; charset=utf-8" };
let faultInjection = false;
let server;
let sessionId;
const results = [];

async function command(method, path, body) {
  const response = await fetch(new URL(path, driverUrl), {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(1_500_000),
  });
  const json = await response.json().catch(() => ({}));
  if (!response.ok || json.value?.error) throw Error(`${method} ${path}: ${JSON.stringify(json.value || json)}`);
  return json.value ?? json;
}

async function evaluate(expression, timeout = 120_000) {
  const script = `const done=arguments[arguments.length-1]; Promise.resolve().then(()=>(${expression})).then(value=>done({ok:true,value}),error=>done({ok:false,error:String(error?.stack||error)}));`;
  const old = await command("POST", `/session/${sessionId}/timeouts`, { script: timeout, pageLoad: 120_000 });
  void old;
  const result = await command("POST", `/session/${sessionId}/execute/async`, { script, args: [] });
  if (!result?.ok) throw Error(result?.error || "Browser script returned no result");
  return result.value;
}

function suiteBody(name) {
  const modules = {
    "image-versions": "const {runImageVersionTests}=await import('../../tests/e2e/image-version-tests.js'); await runImageVersionTests(window.altoEngine, line=>console.log(line));",
    functional: "const {runTests}=await import('../../tests/e2e/browser-tests.js'); await runTests(window.altoEngine,false,line=>console.log(line),{decodeFaultInjection:true});",
    m5: "const {runM5Tests}=await import('../../tests/e2e/m5-tests.js'); await runM5Tests(window.altoEngine,line=>console.log(line));",
    m6: "const {runM6Tests}=await import('../../tests/e2e/m6-tests.js'); await runM6Tests(window.altoEngine,line=>console.log(line));",
    m7: "const {runM7Tests}=await import('../../tests/e2e/m7-tests.js'); await runM7Tests(window.altoEngine,line=>console.log(line));",
    m8: "const {runM8Tests}=await import('../../tests/e2e/m8-tests.js'); await runM8Tests(window.altoEngine,line=>console.log(line));",
    embed: "const {runEmbedTests}=await import('../../tests/e2e/embed-tests.js'); await runEmbedTests(window.altoEngine,line=>console.log(line));",
    parity: "const {runParityTests}=await import('../../tests/e2e/parity-tests.js'); await runParityTests(window.altoEngine,line=>console.log(line));",
  };
  return modules[name];
}

try {
  await mkdir(reportDir, { recursive: true });
  server = createServer(async (req, res) => {
    try {
      const url = new URL(req.url, "http://127.0.0.1");
      const pathname = decodeURIComponent(url.pathname);
      if (pathname === "/__alto-test/decode-fault") {
        faultInjection = url.searchParams.get("enabled") === "1";
        res.writeHead(200).end("ok");
        return;
      }
      let path = resolve(root, "." + pathname);
      if (path !== root && !path.startsWith(root + sep)) return res.writeHead(403).end();
      if ((await stat(path)).isDirectory()) path = join(path, "index.html");
      res.setHeader("Content-Type", mime[extname(path)] || "application/octet-stream");
      if (faultInjection && path === join(root, "packages/alto-sdk/dist/decode-worker.js")) {
        const source = await readFile(path, "utf8");
        const target = "const out = decodeBatch(new Uint8Array(data.batch));";
        if (source.split(target).length !== 2) throw Error("decode fault injection target changed");
        res.setHeader("Cache-Control", "no-store");
        res.end(source.replace(target, "const input = new Uint8Array(data.batch).slice(); if (input.length >= 42) new DataView(input.buffer).setUint32(38, 0xffffffff, true); const out = decodeBatch(input);"));
      } else createReadStream(path).on("error", () => res.destroy()).pipe(res);
    } catch { res.writeHead(404).end(); }
  });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  const pageUrl = `http://127.0.0.1:${server.address().port}/examples/browser-playground/`;

  const capabilities = browser === "safari"
    ? { alwaysMatch: { browserName: "safari" } }
    : { alwaysMatch: { browserName: "firefox", "moz:firefoxOptions": { prefs: { "dom.webgpu.enabled": true, "dom.webgpu.force-enabled": true } } } };
  const created = await command("POST", "/session", { capabilities });
  sessionId = created.sessionId || created.value?.sessionId;
  if (!sessionId) throw Error("WebDriver did not return a session id: " + JSON.stringify(created));
  const caps = created.capabilities || created.value?.capabilities || {};
  console.log(`${browser} ${caps.browserVersion || caps.version || "version unknown"}`);
  await command("POST", `/session/${sessionId}/url`, { url: pageUrl });
  let ready = false;
  for (let i = 0; i < 240; i++) {
    ready = await evaluate("!!window.altoEngine && !!navigator.gpu").catch(() => false);
    if (ready) break;
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  if (!ready) {
    const error = await evaluate("document.querySelector('#error')?.textContent || ''").catch(() => "");
    throw Error(`Engine did not start in ${browser}; WebGPU available=${await evaluate("!!navigator.gpu").catch(() => false)} ${error}`);
  }
  const adapter = await evaluate("(async()=>{const info=(await navigator.gpu.requestAdapter())?.info;return info?{vendor:info.vendor,architecture:info.architecture,device:info.device,description:info.description}:null})()");
  console.log("WebGPU adapter", JSON.stringify(adapter));
  await evaluate("(async()=>{if(!document.querySelector('#recovery').hidden)document.querySelector('#discard').click()})()");
  for (let i = 0; i < 120; i++) {
    if (await evaluate("!!window.altoEngine.document")) break;
    await new Promise((resolve) => setTimeout(resolve, 250));
    if (i === 119) throw Error("Document did not open");
  }
  for (const name of selected) {
    for (let i = 0; i < 120; i++) {
      const state = await evaluate("window.altoEngine?.status?.state||''").catch(() => "");
      if (state === "ready") break;
      if (state === "failed" || state === "disposed") throw Error(`Playground engine is ${state} before ${name}`);
      await new Promise((resolve) => setTimeout(resolve, 250));
      if (i === 119) throw Error(`Playground engine stayed ${state} before ${name}`);
    }
    console.log("RUN", name);
    await evaluate("(async()=>{document.querySelector('#test-results').dataset.report=''})()");
    let error = null;
    try {
      await evaluate(`(async()=>{${suiteBody(name)}})()`, 1_200_000);
    } catch (e) { error = String(e); }
    const report = await evaluate("document.querySelector('#test-results').dataset.report||''").catch(() => "");
    const parsed = report ? JSON.parse(report) : null;
    if (report) await writeFile(join(reportDir, `${name}.json`), report.endsWith("\n") ? report : report + "\n");
    const rows = parsed?.results || [];
    const failed = rows.filter((row) => row.pass === false);
    const ok = !error && rows.length > 0 && failed.length === 0;
    results.push({ name, ok, count: rows.length, failed: failed.length, ...(error ? { error } : {}) });
    console.log(`${name}: ${ok ? "PASS" : "FAIL"} (${rows.length} cases, ${failed.length} failed)${error ? ` ${error}` : ""}`);
    await writeFile(join(reportDir, "summary.json"), JSON.stringify({ browser, version: caps.browserVersion || null, adapter, results }, null, 2) + "\n");
    if (!ok) break;
  }
} catch (error) {
  console.error("WebDriver browser verification failed:", error);
  results.push({ name: "startup", ok: false, error: String(error) });
} finally {
  if (sessionId) await command("DELETE", `/session/${sessionId}`).catch((error) => console.error("Could not close WebDriver session:", error));
  if (server) await new Promise((resolve) => server.close(resolve));
}
console.log(JSON.stringify(results, null, 2));
if (!results.length || results.some((result) => !result.ok)) process.exitCode = 1;
