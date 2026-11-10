// QA-5: ends a real tab at a wait point and checks what a new tab recovers from the
// same OPFS store. Runs in Node, from scripts/verify-browser.mjs, over CDP; the page
// side is tests/e2e/crash-page.html. A tab is ended two ways: `Page.crash` kills the
// renderer without unload, and `Target.closeTarget` closes the tab as a user would.
//
// Wait points (plan D, first two rows):
// - stroke-open: a stroke has drawn but not ended. Recovery gives the last durable state.
// - applied:     edits are applied and their journal write is held in memory.
//                Recovery gives the last durable state, not the applied one.
// - durable:     edits applied and durable() resolved. Recovery gives them back.
// - journal-*:   persist a partial header or payload, or a record with bad CRC,
//                then stop before confirming the append. Recovery must discard it.
//                A second crash checks a new durable edit after tail repair.
// Recovery is compared by snapshot hash, state hash, the layer's tile map and the
// version log. Restoring without error is not a pass.

const POINTS = ["stroke-open", "applied", "durable", "journal-header", "journal-payload", "journal-crc"];
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

  for (const point of POINTS)
    for (const method of METHODS)
      for (let i = 0; i < repeat; i++) {
        const name = `${point} / ${method} #${i + 1}`;
        const namespace = `test-crash/${run}/${point}-${method.split(".")[1]}-${i}`;
        const started = performance.now();
        let error = null;
        let first = null, second = null;
        try {
          first = await open();
          const prepared = await first.evaluate(
            `window.altoCrash.prepare(${JSON.stringify(point)}, ${JSON.stringify(namespace)}, ${i + 1})`,
          );
          await end(first, method);
          first = null;
          second = await open();
          const recovered = await second.evaluate(
            `window.altoCrash.recover(${JSON.stringify(namespace)}, ${JSON.stringify(prepared.layer)})`,
          );
          const differs = Object.keys(prepared.expected).filter((key) => recovered[key] !== prepared.expected[key]);
          if (differs.length)
            throw Error(
              "recovered state differs in " + differs.join(", ") +
              (prepared.lost && recovered.hash === prepared.lost.hash ? " (it holds the edits that were never durable)" : "") +
              ": " + JSON.stringify({ expected: pick(prepared.expected, differs), recovered: pick(recovered, differs) }),
            );
          if (point.startsWith("journal-")) {
            const edited = await second.evaluate(`window.altoCrash.editAfterRecovery(${JSON.stringify(prepared.layer)})`);
            await end(second, method);
            second = await open();
            const again = await second.evaluate(
              `window.altoCrash.recover(${JSON.stringify(namespace)}, ${JSON.stringify(prepared.layer)})`,
            );
            const changed = Object.keys(edited).filter((key) => again[key] !== edited[key]);
            if (changed.length) throw Error("post-recovery durable edit differs in " + changed.join(", "));
          }
        } catch (e) {
          error = String(e?.message || e);
        } finally {
          if (second) {
            await second.evaluate(`window.altoCrash.cleanUp(${JSON.stringify(namespace)})`).catch(() => {});
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
