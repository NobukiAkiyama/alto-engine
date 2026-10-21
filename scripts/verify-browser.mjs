// Drives the Playground's existing suites against the WASM just built into the tree.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { platform } from "node:os";

function chromeBin() {
  if (process.env.CHROME) return process.env.CHROME;
  const candidates =
    platform() === "darwin"
      ? ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"]
      : platform() === "win32"
        ? ["C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"]
        : [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
          ];
  return candidates.find((path) => existsSync(path)) || candidates[0];
}

const chrome = chromeBin();
const port = 9333;
const profile = "/tmp/alto-verify-chrome";
const pageUrl = "http://127.0.0.1:8765/examples/browser-playground/";

const proc = spawn(
  chrome,
  [
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profile}`,
    "--headless=new",
    "--enable-unsafe-webgpu",
    ...(platform() === "darwin" ? ["--use-angle=metal"] : []),
    "--no-first-run",
    "--disable-gpu-sandbox",
    "--window-size=1280,800",
    "about:blank",
  ],
  { stdio: "ignore" },
);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function browserWs() {
  for (let i = 0; i < 50; i++) {
    try {
      const version = await fetch(`http://127.0.0.1:${port}/json/version`).then(
        (r) => r.json(),
      );
      const pages = await fetch(`http://127.0.0.1:${port}/json/list`).then((r) =>
        r.json(),
      );
      const page = pages.find((p) => p.type === "page");
      return { version, page };
    } catch {
      await sleep(200);
    }
  }
  throw new Error("Chrome debugging port did not open");
}

function connect(url) {
  const ws = new WebSocket(url);
  let id = 0;
  const pending = new Map();
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    }
  });
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const my = ++id;
      pending.set(my, resolve);
      ws.send(JSON.stringify({ id: my, method, params }));
      setTimeout(() => reject(new Error("cdp timeout " + method)), 600000);
    });
  return new Promise((resolve, reject) => {
    ws.addEventListener("open", () => resolve({ ws, send }));
    ws.addEventListener("error", () => reject(new Error("websocket failed")));
  });
}

const { version, page } = await browserWs();
console.log(version.Browser);
const { send } = await connect(page.webSocketDebuggerUrl);
await send("Page.enable");
await send("Runtime.enable");
const nav = await send("Page.navigate", { url: pageUrl });
if (nav.result?.errorText) throw new Error(nav.result.errorText);
for (let i = 0; i < 80; i++) {
  const ready = await send("Runtime.evaluate", {
    expression: "!!window.altoEngine && !!navigator.gpu",
    returnByValue: true,
  });
  if (ready.result?.result?.value) break;
  await sleep(250);
  if (i === 59) throw new Error("engine did not start");
}

async function run(expression) {
  const msg = await send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  const details = msg.result?.exceptionDetails;
  if (details) {
    throw new Error(details.text + " " + (details.exception?.description || ""));
  }
  return msg.result?.result?.value;
}

await run(`
  if (!document.querySelector('#recovery').hidden)
    document.querySelector('#discard').click();
  'ok'
`);
await sleep(500);

const suites = [
  ["functional", `const { runTests } = await import('../../tests/e2e/browser-tests.js'); await runTests(window.altoEngine, false, (line) => console.log(line));`],
  ["m5", `const { runM5Tests } = await import('../../tests/e2e/m5-tests.js'); await runM5Tests(window.altoEngine, (line) => console.log(line));`],
  ["m6", `const { runM6Tests } = await import('../../tests/e2e/m6-tests.js'); await runM6Tests(window.altoEngine, (line) => console.log(line));`],
  ["m7", `const { runM7Tests } = await import('../../tests/e2e/m7-tests.js'); await runM7Tests(window.altoEngine, (line) => console.log(line));`],
  ["m8", `const { runM8Tests } = await import('../../tests/e2e/m8-tests.js'); await runM8Tests(window.altoEngine, (line) => console.log(line));`],
  ["benchmark", `const { runTests } = await import('../../tests/e2e/browser-tests.js'); await runTests(window.altoEngine, true, (line) => console.log(line));`],
];

await mkdir("verification", { recursive: true });
const summary = [];
for (const [name, body] of suites) {
  console.log("RUN", name);
  try {
    await run(`(async () => { ${body} })()`);
    const report = await run(
      `document.querySelector('#test-results').dataset.report || ''`,
    );
    const out = `verification/rebuild-${name}.json`;
    await writeFile(out, report.endsWith("\n") ? report : report + "\n");
    const parsed = JSON.parse(report);
    const failed = (parsed.results || []).filter((r) => r.pass === false);
    console.log(name, "results", (parsed.results || []).length, "failed", failed.length);
    summary.push({ name, ok: failed.length === 0, count: (parsed.results || []).length });
  } catch (error) {
    console.log("FAIL", name, error.message);
    summary.push({ name, ok: false, error: String(error.message) });
    break;
  }
}
console.log(JSON.stringify(summary, null, 2));
proc.kill();
process.exit(summary.every((s) => s.ok) ? 0 : 1);
