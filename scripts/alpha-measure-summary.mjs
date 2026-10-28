// Turn a measured-not-enforced alpha parity JSON file into markdown.
// Rust writes { cases: [...] }. The browser parity report writes { alphaMeasure: [...] }.
import { isV3IntegerCase } from "../tests/e2e/cpu-composite.js";
import { mkdir, readFile, writeFile } from "node:fs/promises";

const [backendLabel, jsonPath, outDir] = process.argv.slice(2);
if (!backendLabel || !jsonPath || !outDir) {
  console.error("usage: alpha-measure-summary.mjs <backend-label> <json> <out-dir>");
  process.exit(2);
}

const report = JSON.parse(await readFile(jsonPath, "utf8"));
const rows = report.cases || report.alphaMeasure || [];
if (!rows.length) {
  console.error(`no measured-not-enforced rows in ${jsonPath}`);
  process.exit(1);
}
const backend = report.backend || backendLabel;

function rgba(px) {
  return px.join(",");
}
function sources(pixel) {
  if (!pixel.sources?.length) return "none";
  return pixel.sources.map(rgba).join(" | ");
}
function cell(row) {
  const shown = (row.mismatches || []).slice(0, 3).map((pixel) =>
    `(${pixel.x},${pixel.y}) source ${sources(pixel)} gpu ${rgba(pixel.gpu)} cpu ${rgba(pixel.cpu)}`,
  );
  return shown.length ? shown.join("; ") : "none";
}

let markdown = `### Semi-transparent parity (${backendLabel})\n\n`;
markdown += "These cases are **measured-not-enforced**. A mismatch does not fail the job.\n\n";
markdown += `Backend: \`${backend}\`\n\n`;
markdown += "| case | mismatched pixels | max abs r,g,b,a | first mismatches |\n";
markdown += "| --- | ---: | --- | --- |\n";
for (const row of rows) {
  const maxAbs = (row.maxAbs || [0, 0, 0, 0]).join(",");
  markdown += `| ${row.name} | ${row.count} | ${maxAbs} | ${cell(row)} |\n`;
}
markdown += "\n";
for (const row of rows) {
  if (!row.mismatches?.length) continue;
  markdown += `#### ${row.name}\n\n`;
  for (const pixel of row.mismatches) {
    markdown += `- pixel ${pixel.x},${pixel.y} source ${sources(pixel)} gpu ${rgba(pixel.gpu)} cpu ${rgba(pixel.cpu)}\n`;
  }
  markdown += "\n";
}

const v3Rows = report.compositeV3 || (report.label === "enforced" ? report.cases : []) || [];
const integerRows = v3Rows.filter(isV3IntegerCase);
const unsupportedRows = [...v3Rows.filter((row) => !isV3IntegerCase(row)), ...(report.v3Unsupported || [])];
if (unsupportedRows.length) {
  markdown += `### v3-unsupported (${backendLabel})\n\nRefused by the host (no integer definition yet); excluded from the v3 integer-exact count.\n\n`;
  for (const row of unsupportedRows) markdown += `- ${row.name}: ${row.status}, mismatched pixels ${row.count ?? "error"}\n`;
  markdown += "\n";
}

if (integerRows.length) {
  markdown += `### v3 integer composite (${backendLabel})\n\nEnforced: every byte must match the CPU reference, including cached pyramid levels >=1.\n\n`;
  markdown += "| case | status | mismatched pixels | max abs r,g,b,a | first mismatches |\n| --- | --- | ---: | --- | --- |\n";
  for (const row of integerRows) {
    markdown += `| ${row.name} | ${row.status} | ${row.count ?? "error"} | ${(row.maxAbs || []).join(",")} | ${row.error || cell(row)} |\n`;
  }
  markdown += "\n";
}

const atlas = report.atlasRead;
if (atlas) {
  markdown += `### Atlas read (\`Rgba8Unorm\` + \`textureLoad\` + \`unorm_byte\`, ${backendLabel})\n\n`;
  markdown += "Enforced: a mismatch or error fails the parity run. All 256 byte values on all 4 channels.\n\n";
  markdown += `Status: **${atlas.status}**, mismatched channels: ${atlas.mismatchedChannels}`;
  if (atlas.error) markdown += `, error: ${atlas.error}`;
  markdown += "\n\n";
  for (const m of (atlas.mismatches || []).slice(0, 16)) {
    markdown += `- texel ${m.texel} channel ${m.channel} byte ${m.byte} read ${m.read}\n`;
  }
  markdown += "\n";
}

await mkdir(outDir, { recursive: true });
if (v3Rows.length) {
  await writeFile(`${outDir}/composite-v3.json`, JSON.stringify({ label: "enforced", backend, status: integerRows.some((r) => r.status === "error") ? "error" : integerRows.some((r) => r.status !== "exact" || r.count !== 0) ? "mismatch" : "exact", integerExactCount: integerRows.filter((r) => r.status === "exact" && r.count === 0).length, v3Unsupported: unsupportedRows, cases: integerRows }, null, 2) + "\n");
}
if (unsupportedRows.length) await writeFile(`${outDir}/v3-unsupported.json`, JSON.stringify({ label: "refused", backend, cases: unsupportedRows }, null, 2) + "\n");
if (report.tie8192?.length) {
  const totals = { tiesReached: 0, gpuEqualsCpu: 0, gpuDiffersCpu: 0, unavailable: 0 };
  for (const row of report.tie8192) for (const key of Object.keys(totals)) totals[key] += row[key];
  const tie = { label: "measured-not-enforced", source: "resolve-input-q14", qSource: "cpu-replay-through-f16-store", byteSource: "gpu_tile-readback", cpuByteSource: "cpu-reference", channelOrder: "rgba (adjustOutput8192: rgb)", lossyScope: "input RGBA and adjust output RGB stores; steps 2/4/8", backend, totals, cases: report.tie8192 };
  await writeFile(`${outDir}/tie-8192.json`, JSON.stringify(tie, null, 2) + "\n");
  let tieMarkdown = `### Resolve unorm8 ties (${backendLabel})\n\nMeasured-not-enforced. Q14 is CPU-replayed through the adjustment output f16 store and resolve textureLoad; bytes are GPU tile readback and CPU reference. Tie: \`255*q mod 16384 == 8192\`. Old counts are RGB before the adjustment output f16 store. JSON includes every tie and the old-8192 resolve histogram.\n\nTotals (channels): ties reached **${totals.tiesReached}**, GPU == CPU **${totals.gpuEqualsCpu}**, GPU != CPU **${totals.gpuDiffersCpu}**, unavailable **${totals.unavailable}**.\n\n| case | resolve tie pixels | channels r,g,b,a | ties reached | GPU == CPU | GPU != CPU | old 8192 pixels / r,g,b |\n| --- | ---: | --- | ---: | ---: | ---: | --- |\n`;
  for (const row of tie.cases) tieMarkdown += `| ${row.name} | ${row.resolveInput.pixels} | ${row.resolveInput.channels.join(",")} | ${row.tiesReached} | ${row.gpuEqualsCpu} | ${row.gpuDiffersCpu} | ${row.adjustOutput8192.pixels} / ${row.adjustOutput8192.channels.join(",")} |\n`;
  await writeFile(`${outDir}/tie-8192.md`, tieMarkdown);
  markdown += tieMarkdown;
}

if (atlas) await writeFile(`${outDir}/atlas-read.json`, JSON.stringify({ backend, ...atlas }, null, 2) + "\n");
await writeFile(`${outDir}/alpha-measure.md`, markdown);
await writeFile(`${outDir}/alpha-measure.json`, JSON.stringify({
  label: "measured-not-enforced",
  backend,
  cases: rows,
}, null, 2) + "\n");
if (process.env.GITHUB_STEP_SUMMARY) {
  const { appendFile } = await import("node:fs/promises");
  await appendFile(process.env.GITHUB_STEP_SUMMARY, markdown);
}
console.log(`wrote ${rows.length} measured-not-enforced cases for ${backend}`);

if (integerRows.some((r) => r.status !== "exact" || r.count !== 0)) process.exitCode = 1;
