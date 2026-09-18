import { readdir, stat } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { readJson, text } from "./report.mjs";

const number = (n) => (n == null ? "—" : Math.round(n).toLocaleString("en-US"));
const duration = (ms) => {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
};
const mark = (receipt) =>
  !receipt
    ? "·"
    : receipt.status === "passed"
      ? "✓"
      : receipt.status === "skipped"
        ? "–"
        : receipt.failure_kind === "inconclusive"
          ? "?"
          : "✗";

export function usageFromEvidence(documents) {
  let input = 0,
    output = 0,
    calls = 0,
    tools = 0,
    known = 0,
    knownInput = 0,
    latest = 0;
  for (const { name, data, modified } of documents) {
    if (name.endsWith("-inference.json")) {
      latest = Math.max(latest, modified);
      for (const call of data.InferenceCall || []) {
        calls += 1;
        if (Number.isFinite(call.prompt_tokens)) {
          input += call.prompt_tokens;
          knownInput += 1;
        }
        if (Number.isFinite(call.completion_tokens)) {
          output += call.completion_tokens;
          known += 1;
        }
      }
    }
    if (name.endsWith("-tools.json"))
      tools += (data.AgentToolCall || []).length;
  }
  return {
    input: knownInput ? input : null,
    output: known ? output : null,
    calls,
    tools,
    latest,
  };
}

async function trialSnapshot(directory, model, trial, caseIds, cache) {
  const evidence = join(directory, "evidence");
  let names;
  try {
    names = await readdir(evidence);
  } catch (error) {
    if (error.code === "ENOENT")
      return {
        model,
        trial,
        cases: caseIds.map(() => undefined),
        current: "queued",
        usage: usageFromEvidence([]),
        events: [],
      };
    throw error;
  }
  const documents = [];
  for (const name of names.filter(
    (name) =>
      /-(acceptance|input|inference|tools)\.json$/.test(name) ||
      name === "trial.json",
  )) {
    const path = join(evidence, name);
    try {
      const metadata = await stat(path);
      const previous = cache.get(path);
      let document =
        previous?.stamp === `${metadata.mtimeMs}:${metadata.size}`
          ? previous.document
          : null;
      if (!document) {
        const data = await readJson(path);
        if (!data) continue;
        document = { name, data, modified: metadata.mtimeMs };
        cache.set(path, {
          stamp: `${metadata.mtimeMs}:${metadata.size}`,
          document,
        });
      }
      documents.push(document);
    } catch (error) {
      // Evidence can be observed between truncation and publication.
      if (!(error instanceof SyntaxError) && error.code !== "ENOENT")
        throw error;
      if (cache.has(path)) documents.push(cache.get(path).document);
    }
  }
  const finished = documents.find(({ name }) => name === "trial.json")?.data;
  const receipts = documents.filter(({ name }) =>
    name.endsWith("-acceptance.json"),
  );
  const inputs = documents
    .filter(({ name }) => name.endsWith("-input.json"))
    .sort((a, b) => b.modified - a.modified);
  const current = finished
    ? finished.passed
      ? "passed"
      : "non-pass"
    : inputs[0]?.data.stage || "starting";
  return {
    model,
    trial,
    current,
    finished: Boolean(finished),
    stageStarted: inputs[0]?.modified,
    cases: caseIds.map(
      (id) =>
        receipts.find(({ data }) => data.case_id === id)?.data ||
        finished?.cases.find((entry) => entry.case_id === id),
    ),
    usage: usageFromEvidence(documents),
    events: receipts.map(({ data, modified }) => ({
      ...data,
      modified,
      model,
      trial,
    })),
  };
}

export async function snapshotRun(directory, cache = new Map()) {
  const [report, execution] = await Promise.all([
    readJson(join(directory, "report.json")),
    readJson(join(directory, "execution.json")),
  ]);
  if (!report) return { execution, trials: [], cases: [], directory };
  if (report.schema_version !== 1)
    throw new Error("Unsupported eval report version");
  const cases = report.summaries[0]?.cases.map((entry) => entry.case_id) || [];
  const trials = await Promise.all(
    report.models.flatMap((model, index) =>
      Array.from({ length: report.runs_per_model }, (_, trial) =>
        trialSnapshot(
          join(
            directory,
            "trials",
            `model-${String(index + 1).padStart(3, "0")}-trial-${String(trial + 1).padStart(3, "0")}`,
          ),
          model,
          trial + 1,
          cases,
          cache,
        ),
      ),
    ),
  );
  return { directory, report, execution, cases, trials };
}

export function renderDashboard(
  snapshot,
  { now = Date.now(), columns = 120, rows = 40, color = false } = {},
) {
  const { report, execution, trials, cases } = snapshot;
  if (execution?.finished_at) now = Date.parse(execution.finished_at);
  const paint = (code, value) =>
    color ? `\x1b[${code}m${value}\x1b[0m` : value;
  const elapsed =
    now -
    Date.parse(
      execution?.started_at ||
        report?.started_at ||
        new Date(now).toISOString(),
    );
  const ended = execution && execution.status !== "running";
  const complete = trials.filter((trial) => trial.finished).length;
  const active = trials.filter(
    (trial) => !trial.finished && trial.current !== "queued",
  ).length;
  const totals = trials.reduce(
    (sum, trial) => ({
      input: sum.input + (trial.usage.input || 0),
      output: sum.output + (trial.usage.output || 0),
      tools: sum.tools + trial.usage.tools,
      calls: sum.calls + trial.usage.calls,
    }),
    { input: 0, output: 0, tools: 0, calls: 0 },
  );
  const known = trials.some((trial) => trial.usage.output !== null);
  const knownInput = trials.some((trial) => trial.usage.input !== null);
  const lines = [
    "GENTS  /  ONBOARDING EVAL",
    `${ended ? execution.status.toUpperCase() : "RUNNING"}  ${duration(elapsed)}   ${complete}/${report?.planned || "?"} trials finished   ${ended ? 0 : active} active`,
  ];
  if (!report)
    lines.push(
      "Building / starting eval…  Compiler diagnostics are in runner.log.",
    );
  else {
    lines.push(
      `${report.models.map(text).join(" · ")}   n=${report.runs_per_model}   concurrency=${report.concurrency}${report.stage_timeout_secs ? `   stage budget=${duration(report.stage_timeout_secs * 1000)}` : ""}`,
    );
    lines.push(
      `Reported tokens  IN ${number(knownInput ? totals.input : null)}  OUT ${number(known ? totals.output : null)}   |   ${totals.calls} inference calls   ${totals.tools} saved tool calls`,
    );
    lines.push(
      `Recorded output / wall time: ${known && elapsed > 0 ? (totals.output / (elapsed / 1000)).toFixed(1) : "—"} tok/s (checkpointed; not decode speed)`,
    );
    lines.push(
      "STAGES  " + cases.map((name, i) => `${i + 1} ${name}`).join(" · "),
    );
    const multiple = report.models.length > 1;
    lines.push(
      `${multiple ? "Model         " : ""}Trial  ${cases.map((_, i) => i + 1).join(" ")}   Current work          Age     In tok    Out tok  Calls  Sample age`,
    );
    const ordered = [...trials].sort(
      (a, b) =>
        Number(a.finished || a.current === "queued") -
        Number(b.finished || b.current === "queued"),
    );
    const recentLimit = rows < 30 ? 2 : 3;
    const limit = Math.max(1, rows - 12 - recentLimit);
    for (const trial of ordered.slice(0, limit)) {
      const age =
        trial.finished || !trial.stageStarted
          ? "—"
          : duration(now - trial.stageStarted);
      lines.push(
        `${multiple ? text(trial.model).slice(0, 13).padEnd(14) : ""}${String(trial.trial).padStart(3)}    ${trial.cases
          .map(mark)
          .join(" ")
          .padEnd(cases.length * 2 - 1)}   ${text(
          ended && !trial.finished ? "unfinished" : trial.current,
        )
          .slice(0, 20)
          .padEnd(
            20,
          )} ${age.padStart(6)} ${number(trial.usage.input).padStart(10)} ${number(trial.usage.output).padStart(10)} ${String(trial.usage.calls).padStart(5)}  ${trial.usage.latest ? duration(now - trial.usage.latest) : "—"}`,
      );
    }
    if (trials.length > limit)
      lines.push(
        `… ${trials.length - limit} more trials (active trials shown first)`,
      );
    lines.push(
      "",
      "✓ passed   ✗ failed   ? inconclusive   – skipped   · no verdict yet",
      "",
      "RECENT RESULTS",
    );
    for (const event of trials
      .flatMap((trial) => trial.events)
      .sort((a, b) => b.modified - a.modified)
      .slice(0, recentLimit)) {
      lines.push(
        `${mark(event)}  ${multiple ? `${text(event.model)} ` : ""}#${event.trial} ${event.case_id}  ${duration(event.elapsed_ms)}${event.error ? `  ${text(event.error)}` : ""}`,
      );
    }
  }
  lines.push(`Evidence: ${text(snapshot.directory)}`);
  return lines
    .slice(0, Math.max(rows, 1))
    .map((line, i) => {
      const clipped =
        line.length > columns
          ? `${line.slice(0, Math.max(0, columns - 1))}…`
          : line;
      return i === 0
        ? paint("1;36", clipped)
        : clipped.replace(/[✓✗?·–]/g, (symbol) =>
            paint(
              symbol === "✓"
                ? "32"
                : symbol === "✗"
                  ? "31"
                  : symbol === "?"
                    ? "33"
                    : "2",
              symbol,
            ),
          );
    })
    .join("\n");
}

export function startDashboard(
  directory,
  { output = process.stdout, interval = 1000 } = {},
) {
  const cache = new Map();
  const trialChanges = new Map();
  let stopped = false,
    busy = false,
    previous = "",
    pending = Promise.resolve();
  const tty = Boolean(output.isTTY);
  if (tty) output.write("\x1b[?1049h\x1b[?25l");
  const refresh = () => {
    if (stopped || busy) return;
    busy = true;
    pending = pending
      .then(async () => {
        if (stopped) return;
        try {
          const snapshot = await snapshotRun(directory, cache);
          const content = renderDashboard(snapshot, {
            columns: output.columns || 120,
            rows: output.rows || 40,
            color: tty,
          });
          if (tty) output.write(`\x1b[H\x1b[2J${content}\n`);
          else {
            const status = snapshot.execution?.status || "starting";
            if (status !== previous)
              output.write(
                `Eval ${status}: ${text(directory)} (raw output: runner.log)\n`,
              );
            previous = status;
            for (const trial of snapshot.trials) {
              const key = `${trial.model}:${trial.trial}`;
              const state = `${trial.current} ${trial.cases.map(mark).join(" ")} in=${number(trial.usage.input)} out=${number(trial.usage.output)} calls=${trial.usage.calls}`;
              if (trialChanges.get(key) !== state)
                output.write(
                  `${text(trial.model)} #${trial.trial}  ${text(state)}\n`,
                );
              trialChanges.set(key, state);
            }
          }
        } catch (error) {
          output.write(
            `${tty ? "\x1b[H\x1b[2J" : ""}Eval display unavailable: ${text(error.message)}\n`,
          );
        }
      })
      .finally(() => {
        busy = false;
      });
  };
  refresh();
  const timer = setInterval(refresh, interval);
  return async () => {
    if (stopped) return;
    stopped = true;
    clearInterval(timer);
    await pending;
    if (tty) output.write("\x1b[?25h\x1b[?1049l");
  };
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  if (process.argv.length !== 3) {
    process.stderr.write("Usage: node scripts/evals/watch.mjs RUN_DIRECTORY\n");
    process.exitCode = 1;
  } else {
    const directory = resolve(process.argv[2]);
    if (!(await readJson(join(directory, "execution.json"))))
      throw new Error(`No eval run in ${directory}`);
    const stop = startDashboard(directory);
    const close = async () => {
      await stop();
      process.exitCode = 0;
    };
    process.once("SIGINT", close);
    process.once("SIGTERM", close);
  }
}
