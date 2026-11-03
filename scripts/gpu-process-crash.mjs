// Finds Chrome's GPU process crashes in its stderr. The crash is Chrome's, not a test's:
// verify-browser records it in summary.json without failing the job.
const CRASH = "GPU process exited unexpectedly";

/** Calls onCrash({ exitCode, line }) per crash line. A line may span stderr chunks. */
export function createGpuCrashScanner(onCrash) {
  let tail = "";
  const scan = (line) => {
    if (!line.includes(CRASH)) return;
    const exitCode = /exit_code=(-?\d+)/.exec(line)?.[1];
    onCrash({ exitCode: exitCode === undefined ? null : Number(exitCode), line });
  };
  return {
    push(chunk) {
      const lines = (tail + chunk).split("\n");
      tail = lines.pop();
      lines.forEach(scan);
    },
    flush() {
      scan(tail);
      tail = "";
    },
  };
}

/** The summary.json row. `advisory` separates it from the suites, which have a verdict. */
export function gpuCrashRow(crashes) {
  return {
    name: "unexpected GPU process crash",
    advisory: true,
    ok: true,
    count: crashes.length,
    crashes,
    note: "Chrome's GPU process crashed during this run. This is not a test result: ok only means the crash does not fail the job by itself. Failed tests are in the other rows.",
  };
}

/** A run with no suite verdict failed too: a crash row does not count as a suite that ran. */
export function jobFailed(summary) {
  const verdicts = summary.filter((row) => !row.advisory);
  return !verdicts.length || verdicts.some((row) => !row.ok);
}
