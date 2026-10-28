// Turn a measured-not-enforced alpha parity JSON file into markdown.
// Rust writes { cases: [...] }. The browser parity report writes { alphaMeasure: [...] }.
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

const integerRows = report.compositeV3 || [];
if (integerRows.length) {
  markdown += `### v3 integer composite (${backendLabel})\n\nEnforced: every byte must match the CPU reference.\n\n`;
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
if (integerRows.length) {
  await writeFile(`${outDir}/composite-v3.json`, JSON.stringify({ label: "enforced", backend, status: integerRows.some((r) => r.status === "error") ? "error" : integerRows.some((r) => r.status !== "exact" || r.count !== 0) ? "mismatch" : "exact", cases: integerRows }, null, 2) + "\n");
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
