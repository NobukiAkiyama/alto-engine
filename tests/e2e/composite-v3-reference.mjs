// Native Rust tests consume these bytes directly to compare both independent references.
import { alphaMeasureCases, renderCase } from "./cpu-composite.js";
for (const testCase of alphaMeasureCases()) {
  process.stdout.write(`${testCase.name}\n`);
  process.stdout.write(renderCase(testCase, 3));
}
