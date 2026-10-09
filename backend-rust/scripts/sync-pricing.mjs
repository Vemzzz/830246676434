import { writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  MODEL_PRICING,
  PATTERN_PRICING,
  PROVIDER_PRICING,
} from "../../open-sse/providers/pricing.js";

const output = resolve(dirname(fileURLToPath(import.meta.url)), "../pricing.json");
const catalog = {
  canonical: MODEL_PRICING,
  providers: PROVIDER_PRICING,
  patterns: PATTERN_PRICING,
};

await writeFile(output, `${JSON.stringify(catalog, null, 2)}\n`);
console.log(`Generated ${output}`);
