// QA-5: ends a real tab at a wait point and checks what a new tab recovers from the
// same OPFS store. Runs in Node, from scripts/verify-browser.mjs, over CDP; the page
// side is tests/e2e/crash-page.html. A tab is ended two ways: `Page.crash` kills the
// renderer without unload, and `Target.closeTarget` closes the tab as a user would.
//
// Wait points (plan D, rows 1-6):
// - stroke-open: a stroke has drawn but not ended. Recovery gives the last durable state.
// - applied:     edits are applied and their journal write is held in memory.
//                Recovery gives the last durable state, not the applied one.
// - durable:     edits applied and durable() resolved. Recovery gives them back.
// - journal-*:   persist a partial header or payload, or a record with bad CRC,
//                then stop before confirming the append. Recovery must discard it.
//                A second crash checks a new durable edit after tail repair.
// - checkpoint-*: a checkpoint ends after its pack, after a torn journal, or after a
//                flushed journal never confirmed. The first two recover the previous
//                generation and its appends; the third, the new generation. A second
//                crash checks a durable commit (a new checkpoint) after recovery.
// - folder-*:    a `.altoproj` save into an OPFS folder stops before its manifest is
//                replaced, with the new manifest written but not closed, or after it
//                was replaced. Reopening the folder gives the previous version for the
//                first two and the new one for the third. A second crash checks a save
//                into the folder the dead tab left.
// - gc-*:        a GC over an OPFS folder with two unreachable commits stops before its
//                manifest is replaced, with the new manifest written but not closed,
//                after its first deletion, or before its complete marker. Reopening
//                gives the same document, and every commit restores to the same
//                snapshot and tiles. A second GC must finish and leave no garbage; a
//                second crash checks the folder it left.
// - merge-*:     a three-way merge with conflicts is prepared and not finished,
//                finished with its journal write held or torn, or finished and
//                durable. Recovery gives the state before the merge for the first
//                three and the merge for the last, compared with the version status
//                (refs, head) and every commit's content too. The second round merges
//                again or commits on top of the merge, then a second crash checks it.
// Recovery is compared by snapshot hash, state hash, the layer's tile map and the
// version log. Restoring without error is not a pass.

const POINTS = [
  "stroke-open", "applied", "durable", "journal-header", "journal-payload", "journal-crc",
  "checkpoint-pack", "checkpoint-journal", "checkpoint-flushed",
  "folder-before", "folder-mid", "folder-after",
  "gc-before", "gc-mid", "gc-delete", "gc-complete",
  "merge-prepared", "merge-applied", "merge-torn", "merge-durable",
];
const METHODS = ["Page.crash", "Target.closeTarget"];
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * @param {object} cdp
 * @param {(method: string, params?: object, timeoutMs?: number) => Promise<any>} cdp.browser  browser-level CDP send
 * @param {(url: string, onEvent?: (message: any) => void) => Promise<{ send: Function, close: Function }>} cdp.connect
 * @param {string} cdp.wsOrigin  e.g. ws://127.0.0.1:9222
 * @param {string} pageUrl  tests/e2e/crash-page.html on the test server
 * @param {{ repeat?: number, log?: (line: string) => void }} options
 */
export async function runCrashTests({ browser, connect, wsOrigin }, pageUrl, { repeat = 1, log = console.log } = {}) {
  const run = Date.now().toString(36);
  const results = [];

  async function open() {
    const { result } = await browser("Target.createTarget", { url: pageUrl }, 15000);
    const targetId = result.targetId;
    let crashed = null;
    const crashedNow = new Promise((resolve) => (crashed = resolve));
    const page = await connect(`${wsOrigin}/devtools/page/${targetId}`, (message) => {
      if (message.method === "Inspector.targetCrashed") crashed();
    });
    await page.send("Inspector.enable", {}, 5000);
    await page.send("Runtime.enable", {}, 5000);
    const evaluate = async (expression, timeoutMs = 120000) => {
      const message = await page.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, timeoutMs);
      const details = message.result?.exceptionDetails;
      if (details) throw Error(details.exception?.description || details.text);
      return message.result?.result?.value;
    };
    for (let i = 0; ; i++) {
      if (await evaluate("!!window.altoCrashReady").catch(() => false)) break;
      if (i === 200) throw Error("the crash fixture did not load");
      await sleep(50);
    }
    return { targetId, page, evaluate, crashedNow };
  }
  async function close(tab) {
    tab.page.close();
    await browser("Target.closeTarget", { targetId: tab.targetId }, 10000).catch(() => {});
    await gone(tab.targetId);
  }
  async function gone(targetId) {
    for (let i = 0; i < 200; i++) {
      const { result } = await browser("Target.getTargets", {}, 5000);
      if (!result.targetInfos.some((t) => t.targetId === targetId)) return;
      await sleep(50);
    }
    throw Error("the tab did not close");
  }
  async function end(tab, method) {
    if (method === "Page.crash") {
      // The renderer dies before it can answer.
      tab.page.send("Page.crash", {}, 10000).catch(() => {});
      await Promise.race([tab.crashedNow, sleep(10000).then(() => { throw Error("the renderer did not crash"); })]);
      await close(tab);
    } else {
      tab.page.close();
      await browser("Target.closeTarget", { targetId: tab.targetId }, 10000);
      await gone(tab.targetId);
    }
  }

  // ALTO_CRASH_ONLY=<regex> runs only the matching points, for investigating one.
  const only = typeof process !== "undefined" && process.env.ALTO_CRASH_ONLY ? new RegExp(process.env.ALTO_CRASH_ONLY) : null;
  for (const point of POINTS.filter((p) => !only || only.test(p)))
    for (const method of METHODS)
      for (let i = 0; i < repeat; i++) {
        const name = `${point} / ${method} #${i + 1}`;
        const namespace = `test-crash/${run}/${point}-${method.split(".")[1]}-${i}`;
        const started = performance.now();
        let error = null;
        let first = null, second = null, prepared = null;
        try {
          first = await open();
          prepared = await first.evaluate(
            `window.altoCrash.prepare(${JSON.stringify(point)}, ${JSON.stringify(namespace)}, ${i + 1})`,
          );
          await end(first, method);
          first = null;
          second = await open();
          const folder = JSON.stringify(prepared.folder ?? null);
          const gc = point.startsWith("gc-");
          const merging = point.startsWith("merge-");
          const recover = () =>
            `window.altoCrash.recover(${JSON.stringify(namespace)}, ${JSON.stringify(prepared.layer)}, ${folder}, ${gc}, ${merging})`;
          const recovered = await second.evaluate(recover());
          const differs = Object.keys(prepared.expected).filter((key) => recovered[key] !== prepared.expected[key]);
          if (differs.length)
            throw Error(
              "recovered state differs in " + differs.join(", ") +
              (prepared.lost && recovered.hash === prepared.lost.hash && recovered.stateHash === prepared.lost.stateHash
              ? prepared.folder ? " (it holds the other version)" : " (it holds the edits that were never durable)"
              : "") +
              ": " + JSON.stringify({ expected: pick(prepared.expected, differs), recovered: pick(recovered, differs) }),
            );
          if (!/^(stroke-open|applied|durable)$/.test(point)) {
            const commit = point.startsWith("checkpoint-");
            const edited = await second.evaluate(
              `window.altoCrash.editAfterRecovery(${JSON.stringify(prepared.layer)}, ${commit}, ${folder}, ${JSON.stringify(prepared.garbage ?? null)}, ${JSON.stringify(prepared.merge ?? null)})`,
            );
            await end(second, method);
            second = await open();
            const again = await second.evaluate(recover()).catch((e) => { throw Error("after the second crash: " + (e?.message || e)); });
            const changed = Object.keys(edited).filter((key) => again[key] !== edited[key]);
            if (changed.length) throw Error("post-recovery durable edit differs in " + changed.join(", "));
          }
        } catch (e) {
          error = String(e?.message || e);
        } finally {
          if (second) {
            await second.evaluate(
              `window.altoCrash.cleanUp(${JSON.stringify(namespace)}, ${JSON.stringify(prepared?.folder ?? null)})`,
            ).catch(() => {});
            await close(second).catch(() => {});
          }
          if (first) await close(first).catch(() => {});
        }
        const ms = Math.round(performance.now() - started);
        results.push({ name, pass: !error, ms, ...(error ? { error } : {}) });
        log(`${error ? "FAIL" : "PASS"} ${name} ${ms} ms${error ? " " + error : ""}`);
      }
  return { results };
}

function pick(object, keys) {
  return Object.fromEntries(keys.map((key) => [key, object[key]]));
}
