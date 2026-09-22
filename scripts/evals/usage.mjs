import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { snapshotRun } from "./watch.mjs";
import { text } from "./report.mjs";

export function summarizeUsage(snapshot) {
  const trials = snapshot.trials.flatMap((trial) =>
    (trial.stageUsage || []).map((usage) => ({
      model: trial.model,
      trial: trial.trial,
      ...usage,
    })),
  );
  const groups = new Map();
  for (const row of trials) {
    const key = JSON.stringify([row.model, row.stage]);
    if (!groups.has(key))
      groups.set(key, {
        model: row.model,
        stage: row.stage,
        trials: 0,
        calls: 0,
        tools: 0,
        input: null,
        output: null,
        inputReportedCalls: 0,
        outputReportedCalls: 0,
        peakInput: null,
      });
    const sum = groups.get(key);
    sum.trials++;
    for (const field of [
      "calls",
      "tools",
      "inputReportedCalls",
      "outputReportedCalls",
    ])
      sum[field] += row[field];
    for (const field of ["input", "output"])
      if (row[field] !== null) sum[field] = (sum[field] ?? 0) + row[field];
    if (row.peakInput !== null)
      sum.peakInput = Math.max(sum.peakInput ?? 0, row.peakInput);
  }
  return {
    schema_version: 1,
    directory: snapshot.directory,
    stages: [...groups.values()],
    trials,
  };
}

export function renderUsage(usage) {
  const n = (value) => (value === null ? "—" : value.toLocaleString("en-US"));
  const lines = [
    "Reported token usage by model and stage (all observed trials, including failures)",
    "Stage                      Trials Calls     Input    Output  Peak input  Input/Output coverage",
  ];
  for (const model of new Set(usage.stages.map((row) => row.model))) {
    lines.push(text(model));
    for (const row of usage.stages.filter((row) => row.model === model))
      lines.push(
        `${text(row.stage).padEnd(27)} ${String(row.trials).padStart(5)} ${String(row.calls).padStart(5)} ${n(row.input).padStart(9)} ${n(row.output).padStart(9)} ${n(row.peakInput).padStart(11)}  ${row.inputReportedCalls}/${row.calls} / ${row.outputReportedCalls}/${row.calls}`,
      );
  }
  lines.push(
    "Input sums repeated context across calls; it is not unique or uncached prefill.",
    "Missing/running-call usage is unknown, not zero. Cache hits and TTFT are not recorded here.",
    "Use --json for per-trial, per-stage metrics; original evidence is unchanged.",
  );
  return lines.join("\n") + "\n";
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  try {
    if (
      process.argv.length < 3 ||
      process.argv.length > 4 ||
      (process.argv[3] && process.argv[3] !== "--json")
    )
      throw new Error(
        "Usage: node scripts/evals/usage.mjs RUN_DIRECTORY [--json]",
      );
    const result = summarizeUsage(await snapshotRun(resolve(process.argv[2])));
    process.stdout.write(
      process.argv[3]
        ? JSON.stringify(result, null, 2) + "\n"
        : renderUsage(result),
    );
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
