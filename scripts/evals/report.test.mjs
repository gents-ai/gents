import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { assessRun, renderReport } from "./report.mjs";
import { runConfigurator } from "./run-configurator.mjs";
import { readdir } from "node:fs/promises";
import { setTimeout as delay } from "node:timers/promises";

test("stale launcher heartbeat is not represented as a running eval", () => {
  const now = Date.now();
  assert.equal(
    assessRun(
      null,
      {
        status: "running",
        heartbeat_at: new Date(now - 31000).toISOString(),
      },
      now,
    ),
    "stalled",
  );
  assert.equal(
    assessRun(
      null,
      {
        status: "running",
        heartbeat_at: new Date(now).toISOString(),
      },
      now,
    ),
    "running",
  );
});

test(
  "runtime log remains writable after launcher SIGKILL",
  { timeout: 10000 },
  async () => {
    const root = await mkdtemp(join(tmpdir(), "gents-killed-launcher-"));
    const launcher = spawn(
      process.execPath,
      [
        fileURLToPath(new URL("./run-configurator.mjs", import.meta.url)),
        process.execPath,
        "-e",
        'process.kill(process.ppid, "SIGKILL"); setTimeout(() => console.log("survived observer loss"), 100);',
        "--",
      ],
      {
        env: { ...process.env, GENTS_EVAL_ROOT: root },
        stdio: ["ignore", "ignore", "ignore"],
      },
    );
    const [, signal] = await once(launcher, "close");
    assert.equal(signal, "SIGKILL");
    const [name] = await readdir(root);
    let log = "";
    for (let attempt = 0; attempt < 50; attempt++) {
      log = await readFile(join(root, name, "runner.log"), "utf8");
      if (log.includes("survived observer loss")) break;
      await delay(50);
    }
    assert.match(log, /survived observer loss/);
  },
);

const counts = {
  passed: 1,
  failed: 1,
  skipped: 1,
  unreported: 1,
  pass_rate: 0.5,
};
const report = {
  stage_timeout_secs: 1800,
  schema_version: 1,
  status: "running",
  planned: 4,
  completed: 3,
  passed: 1,
  failed: 2,
  unfinished: 1,
  elapsed_ms: 1200,
  summaries: [
    {
      model: "model\x1b[31m",
      counts,
      fixture_or_harness_failures: 1,
      cases: [
        { case_id: "onboarding", counts, failure_kinds: { inconclusive: 1 } },
      ],
    },
  ],
};

test("saved report displays canonical counts without reclassifying inconclusive or unfinished cases", async () => {
  const directory = await mkdtemp(join(tmpdir(), "gents-report-test-"));
  await writeFile(join(directory, "report.json"), JSON.stringify(report));
  const output = await renderReport(directory);
  assert.match(output, /3\/4 completed; 1 passed, 2 failed, 1 unfinished/);
  assert.match(output, /full workflow 50.0%/);
  assert.match(output, /Stage deadline: 1800s/);
  assert.match(output, /inconclusive: 1/);
  assert.match(output, /Fixture\/harness failures: 1/);
  assert.ok(!output.includes("\x1b"));
  await writeFile(
    join(directory, "report.json"),
    JSON.stringify({ ...report, schema_version: 2 }),
  );
  await assert.rejects(renderReport(directory), /Unsupported/);
});

test("run assessment keeps completed, failed, interrupted, unfinished, and stalled outcomes distinct", () => {
  const complete = {
    ...report,
    status: "completed",
    planned: 1,
    completed: 1,
    passed: 1,
    failed: 0,
    unfinished: 0,
  };
  assert.equal(
    assessRun(complete, { status: "exited", exit_code: 0 }),
    "passed",
  );
  assert.equal(
    assessRun(complete, { status: "exited", exit_code: 101 }),
    "failed",
  );
  assert.equal(
    assessRun(report, { status: "interrupted", signal: "SIGINT" }),
    "interrupted",
  );
  assert.equal(
    assessRun(report, { status: "exited", exit_code: 0 }),
    "unfinished",
  );
  assert.equal(
    assessRun(
      { ...report, updated_at: "2026-09-16T00:00:00Z" },
      { status: "running" },
      Date.parse("2026-09-16T00:31:00Z"),
    ),
    "stalled",
  );
  assert.equal(
    assessRun(
      { ...report, updated_at: "2026-09-16T00:30:45Z" },
      { status: "running" },
      Date.parse("2026-09-16T00:31:00Z"),
    ),
    "running",
  );
});

test("report renders provenance without treating missing sampling as zero", async () => {
  const directory = await mkdtemp(join(tmpdir(), "gents-provenance-test-"));
  await writeFile(
    join(directory, "report.json"),
    JSON.stringify({
      ...report,
      models: ["model"],
      provenance: {
        cohort: "new-cohort",
        source: { commit: "abc123", dirty: true },
        grader: { id: "grader-v1", sha256: "def456" },
        inference: {
          endpoint: "http://inference.test/v1",
          requested_reasoning_effort: "high",
          effective_sampling: { temperature: 1, top_p: 0.95, seed: null },
        },
        fixture_sha256: { "fixture.md": "123" },
      },
    }),
  );
  const output = await renderReport(directory);
  assert.match(output, /Cohort: new-cohort/);
  assert.match(output, /Source: abc123 \(dirty\)/);
  assert.match(output, /temperature=1, top_p=0.95, seed=provider default/);
  assert.match(output, /Fixture hashes: 1/);
  assert.match(output, /Requested reasoning effort: high/);
});

async function fakeRun(script, overrides = {}) {
  const root = await mkdtemp(join(tmpdir(), "gents-launch-test-"));
  let output = "";
  const sink = {
    write(chunk) {
      output += chunk.toString();
    },
  };
  const result = await runConfigurator({
    cargo: [process.execPath, "-e", script, "--"],
    env: {
      ...process.env,
      GENTS_EVAL_SUITE: "progressive-configurator",
      ...overrides,
      GENTS_EVAL_ROOT: root,
    },
    stdout: sink,
    stderr: sink,
  });
  const execution = JSON.parse(
    await readFile(join(result.directory, "execution.json"), "utf8"),
  );
  return { ...result, root, output, execution };
}

test("launcher retains diagnostics and exit status before any trial starts", async () => {
  const result = await fakeRun(
    'console.error("compile failed"); process.exit(101)',
  );
  assert.equal(result.exitCode, 101);
  assert.equal(result.execution.exit_code, 101);
  assert.match(result.output, /No trial results published/);
  assert.match(
    await readFile(join(result.directory, "runner.log"), "utf8"),
    /compile failed/,
  );
  assert.ok(
    result.directory.startsWith(join(result.root, "progressive-configurator-")),
  );
});

test("mailbox suite uses the shared launcher and isolated directory", async () => {
  const result = await fakeRun(
    'process.exit(process.env.GENTS_EVAL_SUITE === "monitor-mailbox" ? 101 : 2)',
    { GENTS_EVAL_SUITE: "monitor-mailbox" },
  );
  assert.equal(result.exitCode, 101);
  assert.ok(result.directory.startsWith(join(result.root, "monitor-mailbox-")));
});

test("launcher rejects an unknown suite before starting cargo", async () => {
  await assert.rejects(
    fakeRun("process.exit(0)", { GENTS_EVAL_SUITE: "typo" }),
    /Unsupported eval suite/,
  );
});

test("launcher prints saved results even when the eval fails", async () => {
  const result = await fakeRun(`
    require('node:fs').writeFileSync(require('node:path').join(process.env.GENTS_EVAL_RUN_DIR, 'report.json'), ${JSON.stringify(JSON.stringify(report))});
    process.exit(101);
  `);
  assert.equal(result.exitCode, 101);
  assert.match(result.output, /1 unfinished/);
  assert.match(result.output, /inconclusive: 1/);
});

test("zero process exit cannot claim an eval pass without results", async () => {
  const result = await fakeRun("process.exit(0)");
  assert.equal(result.exitCode, 1);
  assert.equal(result.execution.status, "error");
  assert.match(result.output, /Cannot establish successful eval/);
});

test("launcher enables live gate and accepts a complete passing report", async () => {
  const passing = {
    ...report,
    status: "completed",
    planned: 1,
    completed: 1,
    passed: 1,
    failed: 0,
    unfinished: 0,
    summaries: [],
  };
  const result = await fakeRun(`
    if (process.env.GENTS_LIVE_CONFIG !== '1') process.exit(2);
    require('node:fs').writeFileSync(require('node:path').join(process.env.GENTS_EVAL_RUN_DIR, 'report.json'), ${JSON.stringify(JSON.stringify(passing))});
  `);
  assert.equal(result.exitCode, 0);
  assert.match(result.output, /1\/1 completed/);
});

test("terminated child is reported as interrupted, not as model failure", async () => {
  const result = await fakeRun('process.kill(process.pid, "SIGTERM")');
  assert.equal(result.exitCode, 143);
  assert.equal(result.execution.status, "interrupted");
  assert.equal(result.execution.signal, "SIGTERM");
  assert.match(result.output, /No trial results published/);
});

test(
  "interrupting the launcher forwards cancellation and retains the process receipt",
  { timeout: 15_000 },
  async (t) => {
    const root = await mkdtemp(join(tmpdir(), "gents-interrupt-test-"));
    const child = spawn(
      process.execPath,
      [
        fileURLToPath(new URL("./run-configurator.mjs", import.meta.url)),
        process.execPath,
        "-e",
        'setTimeout(() => process.kill(process.ppid, "SIGINT"), 100); setInterval(() => {}, 1000)',
        "--",
      ],
      {
        env: { ...process.env, GENTS_EVAL_ROOT: root },
        stdio: ["ignore", "pipe", "pipe"],
      },
    );
    t.after(() => child.kill("SIGTERM"));
    let output = "";
    child.stdout.on("data", (chunk) => {
      output += chunk.toString();
    });
    child.stderr.resume();
    const [code] = await once(child, "close");
    assert.equal(code, 130);
    assert.match(output, /Process: interrupted/);
    const directory = output.match(/Eval directory: (.+)\n/)[1];
    const execution = JSON.parse(
      await readFile(join(directory, "execution.json"), "utf8"),
    );
    assert.equal(execution.signal, "SIGINT");
  },
);
