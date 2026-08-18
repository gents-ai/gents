import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { loadPackManifest } from "./interpolate.mjs";

const packRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const port = process.env.REVIEW_PORT || "19191";
const graphql = `http://127.0.0.1:${port}/api/v0/graphql`;

function escapeGraphqlString(value) {
  return String(value)
    .replaceAll("\\", "\\\\")
    .replaceAll('"', '\\"')
    .replaceAll("\n", "\\n")
    .replaceAll("\r", "\\r")
    .replaceAll("\t", "\\t");
}

function jobId() {
  if (process.env.GENTS_REVIEW_JOB_ID) {
    return process.env.GENTS_REVIEW_JOB_ID;
  }
  const stamp = new Date().toISOString().replace(/[-:]/g, "").replace(/\.\d+Z$/, "Z");
  return `review-${stamp}-${process.pid}`;
}

async function healthz() {
  try {
    const response = await fetch(`http://127.0.0.1:${port}/healthz`);
    return response.ok;
  } catch {
    return false;
  }
}

if (!(await healthz())) {
  console.error("start the pack node first: make review-serve");
  process.exit(2);
}

const manifest = loadPackManifest(packRoot);
const seed = manifest.seed;
const runId = jobId();
const prompt = process.env.GENTS_REVIEW_PROMPT || manifest.default_prompt;
const fields = [
  `${seed.job_id_field}: "${escapeGraphqlString(runId)}"`,
  `${seed.prompt_field}: "${escapeGraphqlString(prompt)}"`,
];
for (const [key, value] of Object.entries(seed.fields ?? {})) {
  fields.push(`${key}: "${escapeGraphqlString(value ?? "")}"`);
}
const mutation = `mutation { create_${seed.collection}(input: { ${fields.join(", ")} }) { _docID } }`;

const response = await fetch(graphql, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ query: mutation }),
});
const payload = await response.json();
if (!response.ok || payload.errors?.length) {
  console.error(JSON.stringify(payload.errors ?? payload, null, 2));
  process.exit(1);
}
console.log(`seeded ${seed.collection} run_id=${runId}`);
console.log(`page     http://127.0.0.1:${process.env.REVIEW_PAGE_PORT || "19190"}`);
