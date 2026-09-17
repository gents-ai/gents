import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  renderDashboard,
  snapshotRun,
  startDashboard,
  usageFromEvidence,
  stageUsageFromEvidence,
} from "./watch.mjs";
import { summarizeUsage, renderUsage } from "./usage.mjs";

test("stage usage preserves partial coverage, model isolation and repeated input", () => {
  const stages = stageUsageFromEvidence([
    {
      name: "preview-inference.json",
      modified: 1,
      data: {
        InferenceCall: [
          { prompt_tokens: 100, completion_tokens: 10 },
          { prompt_tokens: 150, completion_tokens: 0 },
          { prompt_tokens: null, completion_tokens: null },
        ],
      },
    },
    { name: "preview-tools.json", data: { AgentToolCall: [{}] } },
    {
      name: "apply-inference.json",
      modified: 2,
      data: { InferenceCall: [{}] },
    },
  ]);
  assert.equal(stages[0].input, 250);
  assert.equal(stages[0].peakInput, 150);
  assert.equal(stages[0].inputReportedCalls, 2);
  assert.equal(stages[0].outputReportedCalls, 2);
  assert.equal(stages[0].calls, 3);
  assert.equal(stages[0].tools, 1);
  assert.equal(stages[1].input, null);
  const result = summarizeUsage({
    directory: "/test",
    trials: [
      { model: "one", trial: 1, stageUsage: stages },
      { model: "one", trial: 2, stageUsage: stages },
      { model: "two", trial: 1, stageUsage: stages },
      { model: "one", trial: 3 },
    ],
  });
  assert.equal(result.stages.length, 4);
  assert.equal(result.stages[0].input, 500);
  assert.equal(result.stages[0].peakInput, 150);
  assert.equal(result.stages[0].inputReportedCalls, 4);
  assert.equal(result.trials.length, 6);
  assert.match(renderUsage(result), /4\/6 \/ 4\/6/);
  assert.match(renderUsage(result), /not unique or uncached prefill/);
});

test("usage counts saved calls once and distinguishes unknown tokens from zero", () => {
  const document = {
    name: "onboarding-inference.json",
    modified: 100,
    data: {
      InferenceCall: [
        { prompt_tokens: 100, completion_tokens: 20 },
        { prompt_tokens: 50, completion_tokens: null },
      ],
    },
  };
  const usage = usageFromEvidence([
    document,
    { name: "onboarding-tools.json", data: { AgentToolCall: [{}, {}] } },
  ]);
  assert.deepEqual(usage, {
    input: 150,
    output: 20,
    calls: 2,
    tools: 2,
    latest: 100,
  });
  assert.equal(usageFromEvidence([]).output, null);
  assert.equal(
    usageFromEvidence([
      {
        ...document,
        data: {
          InferenceCall: [{ prompt_tokens: 100, completion_tokens: null }],
        },
      },
    ]).input,
    100,
  );
  assert.equal(
    usageFromEvidence([
      { ...document, data: { InferenceCall: [{ completion_tokens: 0 }] } },
    ]).output,
    0,
  );
});

async function fixture() {
  const directory = await mkdtemp(join(tmpdir(), "gents-watch-test-"));
  const evidence = join(directory, "trials", "model-001-trial-001", "evidence");
  await mkdir(evidence, { recursive: true });
  await writeFile(
    join(directory, "execution.json"),
    JSON.stringify({
      schema_version: 1,
      status: "running",
      started_at: "2026-09-16T00:00:00Z",
    }),
  );
  await writeFile(
    join(directory, "report.json"),
    JSON.stringify({
      schema_version: 1,
      updated_at: "2026-09-16T00:00:00Z",
      models: ["model"],
      planned: 2,
      unfinished: 2,
      runs_per_model: 2,
      concurrency: 1,
      stage_timeout_secs: 1800,
      summaries: [
        {
          cases: [{ case_id: "onboarding" }, { case_id: "builder-readiness" }],
        },
      ],
    }),
  );
  await writeFile(
    join(evidence, "onboarding-acceptance.json"),
    JSON.stringify({
      case_id: "onboarding",
      status: "failed",
      failure_kind: "inconclusive",
      elapsed_ms: 1500,
      error: "ambiguous\u001b[2J",
    }),
  );
  await writeFile(
    join(evidence, "builder-readiness-input.json"),
    JSON.stringify({ stage: "builder-readiness" }),
  );
  return { directory, evidence };
}

test("watching existing receipts preserves verdicts, pending work and interrupted state without writing", async () => {
  const { directory, evidence } = await fixture();
  const before = await readFile(join(directory, "report.json"), "utf8");
  const cache = new Map();
  const snapshot = await snapshotRun(directory, cache);
  assert.equal(snapshot.trials[1].current, "queued");
  assert.equal(snapshot.trials[0].current, "builder-readiness");
  let view = renderDashboard(snapshot, { columns: 100, rows: 30 });
  assert.match(view, /\? ·/);
  assert.match(view, /checkpointed; not decode speed/);
  assert.ok(!view.includes("\x1b"));
  assert.ok(view.split("\n").every((line) => line.length <= 100));
  assert.equal(await readFile(join(directory, "report.json"), "utf8"), before);
  await writeFile(join(evidence, "onboarding-acceptance.json"), "{");
  assert.equal(
    (await snapshotRun(directory, cache)).trials[0].cases[0].failure_kind,
    "inconclusive",
  );
  snapshot.execution = {
    ...snapshot.execution,
    status: "interrupted",
    finished_at: "2026-09-16T00:01:00Z",
  };
  view = renderDashboard(snapshot, { now: Date.parse("2026-09-16T01:00:00Z") });
  assert.match(view, /INTERRUPTED \/ NON-PASSING  1:00/);
  assert.match(view, /unfinished/);
});

test("stage progress attributes requests and exposes stalled work as non-passing", async () => {
  const { directory, evidence } = await fixture();
  await writeFile(
    join(evidence, "builder-readiness-progress.json"),
    JSON.stringify({
      stage: "builder-readiness",
      phase: "observing",
      request_id: "request-visible-123",
      lifecycle_state: "processing",
      started_at: "2026-09-16T00:00:00Z",
      updated_at: "2026-09-16T00:00:00Z",
    }),
  );
  const snapshot = await snapshotRun(directory);
  assert.equal(snapshot.trials[0].requestId, "request-visible-123");
  const view = renderDashboard(snapshot, {
    now: Date.parse("2026-09-16T00:31:00Z"),
    columns: 160,
    rows: 30,
  });
  assert.match(view, /STALLED \/ NON-PASSING/);
  assert.match(view, /stalled/);
  assert.match(view, /request-visi/);
});

test("TTY display restores the cursor and screen on stop", async () => {
  const { directory } = await fixture();
  let output = "";
  const stop = startDashboard(directory, {
    output: {
      isTTY: true,
      columns: 100,
      rows: 30,
      write(chunk) {
        output += chunk;
      },
    },
  });
  await stop();
  assert.ok(output.startsWith("\x1b[?1049h\x1b[?25l"));
  assert.ok(output.endsWith("\x1b[?25h\x1b[?1049l"));
});
