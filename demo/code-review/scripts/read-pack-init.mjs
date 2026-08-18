import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { loadPackManifest } from "./interpolate.mjs";

const packRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const init = loadPackManifest(packRoot).init;
const field = process.argv[2];
if (!field) {
  process.stdout.write(`${JSON.stringify(init)}\n`);
  process.exit(0);
}
const value = init[field];
if (value === undefined || value === null) {
  process.exit(0);
}
process.stdout.write(`${value}\n`);
