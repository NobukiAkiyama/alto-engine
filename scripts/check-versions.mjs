// Version numbers written in prose and in verification/build.json must match the
// constants the code actually ships. A format or protocol bump changes several
// files by design; this check is what keeps the prose from drifting behind them.
import { readFileSync } from "node:fs";

const failures = [];
const read = (path) => readFileSync(path, "utf8");

const constant = (path, pattern, name) => {
  const match = read(path).match(pattern);
  if (!match) {
    failures.push(`${path}: could not find ${name}`);
    return null;
  }
  return match[1];
};

const engineVersion = constant("crates/alto-wasm/Cargo.toml", /^version = "([^"]+)"/m, "the engine version");
const sdkVersion = JSON.parse(read("packages/alto-sdk/package.json")).version;
const formatVersion = constant("crates/alto-doc/src/lib.rs", /pub const FORMAT_VERSION: u32 = (\d+);/, "FORMAT_VERSION");
const baseFormat = constant("crates/alto-doc/src/lib.rs", /pub const BASE_FORMAT: u32 = (\d+);/, "BASE_FORMAT");
const textFormat = constant("crates/alto-doc/src/lib.rs", /const TEXT_FORMAT: u32 = (\d+);/, "TEXT_FORMAT");
const renderVersion = constant(
  "crates/alto-core/src/lib.rs",
  /pub const RENDER_VERSION_EXACT: u32 = (\d+);/,
  "RENDER_VERSION_EXACT",
);
const sdkProtocol = constant("packages/alto-sdk/src/protocol.ts", /export const PROTOCOL = (\d+);/, "PROTOCOL");
const wasmProtocol = constant(
  "crates/alto-wasm/src/lib.rs",
  /pub fn protocol_version\(&self\) -> u32 \{\s*(\d+)\s*\}/,
  "Runtime::protocol_version",
);
const protocol = sdkProtocol ?? wasmProtocol;

if (sdkProtocol !== null && wasmProtocol !== null && sdkProtocol !== wasmProtocol) {
  failures.push(
    `packages/alto-sdk/src/protocol.ts has PROTOCOL = ${sdkProtocol} but ` +
      `crates/alto-wasm/src/lib.rs reports ${wasmProtocol}`,
  );
}
if ([baseFormat, textFormat, formatVersion].every((format) => format !== null) &&
    !(Number(baseFormat) < Number(textFormat) && Number(textFormat) < Number(formatVersion))) {
  failures.push(`Document formats must be in order: BASE_FORMAT (${baseFormat}), TEXT_FORMAT (${textFormat}), FORMAT_VERSION (${formatVersion})`);
}

const manifestFormat = "2";
const commitFormat = "2";

// Each entry: file, pattern, expected values (in capture order), label.
const claims = [
  [
    "README.md",
    /^# Alto Engine — render v(\d+) \/ format (\d+)（Engine ([\d.]+) \/ TypeScript SDK ([\d.]+) \/ Protocol (\d+)）/m,
    [renderVersion, formatVersion, engineVersion, sdkVersion, protocol],
    "title",
  ],
  [
    "README.md",
    /この開発版は Protocol (\d+) \/ format (\d+) を使います/,
    [protocol, formatVersion],
    "startup note",
  ],
  [
    "README.md",
    /現行のrender v(\d+) \/ format (\d+)に対応した/,
    [renderVersion, formatVersion],
    "M7/M8 sample note",
  ],
  [
    "README.md",
    /TypeScript SDK ([\d.]+)（プロトコル(\d+)）/,
    [sdkVersion, protocol],
    "package table",
  ],
  [
    "README.md",
    /プロトコルは \*\*(\d+)\*\* です/,
    [protocol],
    "protocol note",
  ],
  [
    "README.md",
    /（文書format (\d+) \/ manifest (\d+) \/ commit (\d+) \/ プロトコル(\d+)）/,
    [formatVersion, manifestFormat, commitFormat, protocol],
    "supported-formats summary",
  ],
  [
    "RELEASING.md",
    /Engine\/WASM `([\d.]+)`, TypeScript SDK `([\d.]+)`, wire protocol `(\d+)`, document format `(\d+)`, package manifest format `(\d+)`, and commit format `(\d+)`/,
    [engineVersion, sdkVersion, protocol, formatVersion, manifestFormat, commitFormat],
    "version policy",
  ],
  [
    "RELEASING.md",
    /accepts formats (\d+), (\d+), and (\d+) \/ render v(\d+) only/,
    [baseFormat, textFormat, formatVersion, renderVersion],
    "accepted formats",
  ],
  [
    "doc/MILESTONES.md",
    /^# 実装状況 — render v(\d+) \/ format (\d+)（未リリース）/m,
    [renderVersion, formatVersion],
    "title",
  ],
  [
    "doc/MILESTONES.md",
    /現行の文書formatは (\d+)、manifest は (\d+)、commit は (\d+) です。当時の wire protocol は (\d+) で、現行は (\d+) です/,
    [formatVersion, manifestFormat, commitFormat, null, protocol],
    "protocol history",
  ],
  [
    "doc/MILESTONES.md",
    /受け付ける形式はformat (\d+)、(\d+)、(\d+) \/ manifest (\d+) \/ commit (\d+) \/ render v(\d+)のみです/,
    [baseFormat, textFormat, formatVersion, manifestFormat, commitFormat, renderVersion],
    "accepted formats",
  ],
  [
    "doc/MILESTONES.md",
    /Engine\/WASM ([\d.]+)、TypeScript SDK ([\d.]+)（現行はプロトコル(\d+)）/,
    [engineVersion, sdkVersion, protocol],
    "requirements",
  ],
];

for (const [file, pattern, expected, label] of claims) {
  const match = read(file).match(pattern);
  if (!match) {
    failures.push(`${file}: ${label} is missing or no longer written in the checked form`);
    continue;
  }
  expected.forEach((want, index) => {
    if (want === null) return;
    const actual = match[index + 1];
    if (actual !== String(want)) {
      failures.push(`${file}: ${label} says "${actual}" where the source says "${want}"`);
    }
  });
}

const metadata = JSON.parse(read("verification/build.json"));
if (String(metadata.version) !== String(engineVersion)) {
  failures.push(
    `verification/build.json: version is "${metadata.version}" where the engine is "${engineVersion}"`,
  );
}
if (String(metadata.protocolVersion) !== String(protocol)) {
  failures.push(
    `verification/build.json: protocolVersion is "${metadata.protocolVersion}" where the code is "${protocol}"`,
  );
}

if (failures.length) {
  for (const failure of failures) console.error(failure);
  process.exit(1);
}
console.log(
  `version references match the source: engine ${engineVersion}, SDK ${sdkVersion}, ` +
    `protocol ${protocol}, format ${formatVersion} (also ${baseFormat} and ${textFormat}), render v${renderVersion}`,
);
