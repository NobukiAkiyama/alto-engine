// Native Rust tests consume these bytes directly to compare both independent references.
import { alphaMeasureCases, renderCase, renderPyramidCase } from "./cpu-composite.js";
for (const testCase of alphaMeasureCases()) {
  process.stdout.write(`${testCase.name}\n`);
  process.stdout.write(renderCase(testCase, 3));
}
for (const source of alphaMeasureCases()) {
  const testCase = {...source, level:1, tx:0, ty:0};
  process.stdout.write(`${source.name}-level1\n`);
  process.stdout.write(renderPyramidCase(testCase));
}
