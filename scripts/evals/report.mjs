import { readFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";

export async function readJson(path) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw error;
  }
}

const rate = (value) =>
  value === null ? "n/a" : `${(value * 100).toFixed(1)}%`;
// Model names and other external text must not control the terminal.
export const text = (value) =>
  String(value).replace(/[\x00-\x1f\x7f-\x9f]/g, " ");

export async function renderReport(directory) {
  const [report, execution] = await Promise.all([
    readJson(join(directory, "report.json")),
    readJson(join(directory, "execution.json")),
  ]);
  if (!report && !execution) throw new Error(`No eval report in ${directory}`);
  for (const document of [report, execution]) {
    if (document && document.schema_version !== 1)
      throw new Error("Unsupported eval report version");
  }
  const lines = [`\nConfigurator eval — ${text(directory)}`];
  if (execution) {
    lines.push(
      `Process: ${execution.status}${execution.exit_code !== null ? ` (exit ${execution.exit_code})` : ""}${execution.signal ? ` (${execution.signal})` : ""}`,
    );
    if (execution.error) lines.push(text(execution.error));
  }
  if (!report) {
    lines.push(
      "No trial results published. See runner.log for build/startup diagnostics.",
    );
  } else {
    lines.push(
      `Trials: ${report.completed}/${report.planned} completed; ${report.passed} passed, ${report.failed} failed, ${report.unfinished} unfinished.`,
    );
    lines.push(
      `Eval time: ${(report.elapsed_ms / 1000).toFixed(1)}s (last checkpoint)`,
    );
    if (report.stage_timeout_secs)
      lines.push(
        `Stage deadline: ${report.stage_timeout_secs}s (+30s interrupt grace)`,
      );
    for (const summary of report.summaries) {
      lines.push(
        `\n${text(summary.model)} — full workflow ${rate(summary.counts.pass_rate)}`,
      );
      lines.push(
        "  Case                      Pass  Fail  Skip  Unreported  Pass rate",
      );
      for (const entry of summary.cases) {
        const c = entry.counts;
        lines.push(
          `  ${text(entry.case_id).padEnd(25)} ${String(c.passed).padStart(4)} ${String(c.failed).padStart(5)} ${String(c.skipped).padStart(5)} ${String(c.unreported).padStart(11)} ${rate(c.pass_rate).padStart(10)}`,
        );
        const kinds = Object.entries(entry.failure_kinds);
        if (kinds.length)
          lines.push(
            `    ${kinds.map(([kind, count]) => `${text(kind)}: ${count}`).join(", ")}`,
          );
      }
      if (summary.fixture_or_harness_failures)
        lines.push(
          `  Fixture/harness failures: ${summary.fixture_or_harness_failures}`,
        );
    }
    lines.push(
      "\nPass rates exclude prerequisite skips and unreported cases; failed includes inconclusive checks.",
    );
  }
  lines.push(
    `Evidence: ${text(join(directory, "trials"))}`,
    `Log: ${text(join(directory, "runner.log"))}`,
  );
  return `${lines.join("\n")}\n`;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  try {
    if (process.argv.length !== 3)
      throw new Error("Usage: node scripts/evals/report.mjs RUN_DIRECTORY");
    process.stdout.write(await renderReport(resolve(process.argv[2])));
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
