import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { loadPackManifest } from "./interpolate.mjs";

const packRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const port = process.env.REVIEW_PORT || "19191";
const graphql = `http://127.0.0.1:${port}/api/v0/graphql`;
const home = process.env.REVIEW_HOME || resolve(packRoot, "runs/demo-home");

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

async function getJson(url) {
  const response = await fetch(url, { cache: "no-store" });
  if (!response.ok) {
    throw new Error(`${url} -> ${response.status}`);
  }
  return response.json();
}

async function postGraphql(query) {
  const response = await fetch(graphql, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ query }),
  });
  const payload = await response.json();
  if (!response.ok || payload.errors?.length) {
    throw new Error(JSON.stringify(payload.errors ?? payload));
  }
  return payload.data;
}

async function sleep(ms) {
  await new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
}

async function waitUntil(label, timeoutMs, probe) {
  const started = Date.now();
  let lastError = "";
  while (Date.now() - started < timeoutMs) {
    try {
      if (await probe()) {
        return;
      }
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await sleep(400);
  }
  throw new Error(
    lastError
      ? `${label} timed out after ${Math.round(timeoutMs / 1000)}s: ${lastError}`
      : `${label} timed out after ${Math.round(timeoutMs / 1000)}s`,
  );
}

function stampedReviewRoot() {
  const stamp = resolve(home, "review-root");
  if (!existsSync(stamp)) {
    return null;
  }
  return readFileSync(stamp, "utf8").trim();
}

const envRoot = process.env.GENTS_REVIEW_ROOT;
const appliedRoot = stampedReviewRoot();
if (envRoot && appliedRoot && resolve(envRoot) !== resolve(appliedRoot)) {
  console.error(
    `REVIEW_ROOT ${resolve(envRoot)} does not match the pack node at ${appliedRoot}; re-run make review-serve (REVIEW_RESET=1 if you meant to retarget)`,
  );
  process.exit(2);
}

const manifest = loadPackManifest(packRoot);
const seed = manifest.seed;

try {
  await waitUntil("pack /healthz", 30_000, async () => {
    const response = await fetch(`http://127.0.0.1:${port}/healthz`);
    return response.ok;
  });
  await waitUntil("pack /status behavior_readiness", 120_000, async () => {
    const status = await getJson(`http://127.0.0.1:${port}/status`);
    return status.behavior_readiness === "ready";
  });
  await waitUntil(`EventTrigger on ${seed.collection}`, 60_000, async () => {
    const data = await postGraphql("{ EventTrigger { trigger_id source_collection enabled } }");
    return (data.EventTrigger ?? []).some(
      (trigger) => trigger.source_collection === seed.collection && trigger.enabled,
    );
  });
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  console.error("start the pack node first: make review-serve");
  process.exit(2);
}

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

try {
  await waitUntil(`recon request for ${runId}`, 60_000, async () => {
    const data = await postGraphql(
      `{ AgentRequest(filter: { caused_by_correlation: { _eq: "${escapeGraphqlString(runId)}" } }) { request_id } }`,
    );
    return (data.AgentRequest ?? []).length > 0;
  });
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  console.error(
    "the ReviewJob was written but no request fired; the event source was not observing yet — retry make review",
  );
  process.exit(1);
}

console.log(`seeded ${seed.collection} run_id=${runId}`);
console.log(`page     http://127.0.0.1:${process.env.REVIEW_PAGE_PORT || "19190"}/?run=${runId}`);
