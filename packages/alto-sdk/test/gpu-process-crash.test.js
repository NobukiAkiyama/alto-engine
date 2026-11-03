import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { createGpuCrashScanner, gpuCrashRow, jobFailed } from "../../../scripts/gpu-process-crash.mjs";

const verifyBrowser = fileURLToPath(new URL("../../../scripts/verify-browser.mjs", import.meta.url));
// From the chrome-stderr.log of CI run 41613286724.
const crashLine = "[5398:20527:1103/102412.578126:ERROR:content/browser/gpu/gpu_process_host.cc:1044] GPU process exited unexpectedly: exit_code=8704";

function scan(...chunks) {
  const crashes = [];
  const scanner = createGpuCrashScanner((crash) => crashes.push(crash));
  for (const chunk of chunks) scanner.push(chunk);
  scanner.flush();
  return crashes;
}

test("a crash line in Chrome's stderr is one crash with its exit code", () => {
  assert.deepEqual(scan(`first\n${crashLine}\nlast\n`), [{ exitCode: 8704, line: crashLine }]);
});

test("a crash line split across stderr chunks is still one crash", () => {
  assert.deepEqual(scan(crashLine.slice(0, 90), crashLine.slice(90) + "\n"), [{ exitCode: 8704, line: crashLine }]);
});

test("a crash line the stream ends on is counted at flush", () => {
  assert.equal(scan(crashLine).length, 1);
});

test("Chrome's other GPU process lines are not crashes", () => {
  const others = [
    "[5493:20974:1103/102412.471299:ERROR:components/viz/service/gl/exit_code.cc:13] Restarting GPU process due to unrecoverable error. Context was lost.",
    "[5398:20527:1103/102412.578177:WARNING:content/browser/gpu/gpu_process_host.cc:1503] The GPU process has crashed 1 time(s)",
    "[5398:20527:1103/102412.869985:WARNING:content/browser/gpu/gpu_process_host.cc:1066] Reinitialized the GPU process after a crash. The reported initialization time was 90 ms",
  ];
  assert.deepEqual(scan(others.join("\n") + "\n"), []);
});

test("a crash without an exit code is still a crash", () => {
  assert.deepEqual(scan("GPU process exited unexpectedly\n"), [{ exitCode: null, line: "GPU process exited unexpectedly" }]);
});

test("a crash row alone neither fails the job nor counts as a suite that ran", () => {
  const passed = { name: "parity", ok: true, count: 3 };
  const row = gpuCrashRow([{ exitCode: 8704, line: crashLine, suite: "parity", at: "t" }]);
  assert.equal(row.count, 1);
  assert.equal(jobFailed([passed, row]), false);
  assert.equal(jobFailed([{ ...passed, ok: false }, row]), true);
  assert.equal(jobFailed([row]), true);
  assert.equal(jobFailed([]), true);
});

test("verify-browser records a crash in summary.json apart from the failed startup and warns in CI", () => {
  const dir = mkdtempSync(join(tmpdir(), "alto-gpu-crash-"));
  try {
    // Prints the crash line, then exits before it opens a debugging port: a startup failure.
    const chrome = join(dir, "fake-chrome.sh");
    writeFileSync(chrome, `#!/bin/sh\necho '${crashLine}' >&2\nsleep 1\n`);
    chmodSync(chrome, 0o755);
    const run = spawnSync(process.execPath, [verifyBrowser, "--suite=parity"], {
      encoding: "utf8",
      env: { ...process.env, CHROME: chrome, ALTO_BROWSER_REPORT_DIR: dir, GITHUB_ACTIONS: "true" },
    });
    assert.equal(run.status, 1);
    assert.match(run.stdout, /^::warning title=Unexpected GPU process crash::.*exit_code=8704/m);

    const summary = JSON.parse(readFileSync(join(dir, "summary.json"), "utf8"));
    const failed = summary.filter((row) => !row.ok);
    assert.deepEqual(failed.map((row) => row.name), ["startup"]);
    const row = summary.find((candidate) => candidate.advisory);
    assert.equal(row.name, "unexpected GPU process crash");
    assert.equal(row.count, 1);
    assert.equal(row.crashes[0].exitCode, 8704);
    assert.equal(row.crashes[0].suite, "startup");
    assert.equal(row.crashes[0].line, crashLine);

    const diagnostics = JSON.parse(readFileSync(join(dir, "browser-diagnostics.json"), "utf8"));
    assert.equal(diagnostics.gpuProcessCrashes.length, 1);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
