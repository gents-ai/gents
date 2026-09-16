import { spawn, execFileSync } from "node:child_process";
import { createWriteStream } from "node:fs";
import { mkdir, mkdtemp, readFile, rename, writeFile } from "node:fs/promises";
import { constants, homedir } from "node:os";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { finished } from "node:stream/promises";
import { renderReport } from "./report.mjs";
import { startDashboard } from "./watch.mjs";

function revision(cwd) {
  try {
    const git = (...args) =>
      execFileSync("git", args, {
        cwd,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      }).trim();
    return {
      commit: git("rev-parse", "HEAD"),
      dirty: Boolean(git("status", "--porcelain", "--untracked-files=no")),
    };
  } catch {
    return null;
  }
}

export async function runConfigurator({
  cargo = ["cargo"],
  env = process.env,
  cwd = process.cwd(),
  stdout = process.stdout,
} = {}) {
  const root = resolve(env.GENTS_EVAL_ROOT || join(homedir(), ".gents-eval"));
  await mkdir(root, { recursive: true, mode: 0o700 });
  const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
  const directory = await mkdtemp(
    join(root, `progressive-configurator-${timestamp}-`),
  );
  const execution = {
    schema_version: 1,
    started_at: new Date().toISOString(),
    finished_at: null,
    status: "running",
    exit_code: null,
    signal: null,
    error: null,
    revision: revision(cwd),
  };
  const save = async () => {
    const temporary = join(directory, "execution.json.tmp");
    await writeFile(temporary, `${JSON.stringify(execution, null, 2)}\n`, {
      mode: 0o600,
    });
    await rename(temporary, join(directory, "execution.json"));
  };
  await save();
  stdout.write(`Eval directory: ${directory}\n`);
  const stopDashboard = startDashboard(directory, { output: stdout });
  try {
    const log = createWriteStream(join(directory, "runner.log"), {
      mode: 0o600,
    });
    const child = spawn(
      cargo[0],
      [
        ...cargo.slice(1),
        "test",
        "-p",
        "gents",
        "--test",
        "e2e_configurator",
        "live_configurator_progressive_eval_matrix",
        "--",
        "--ignored",
        "--nocapture",
      ],
      {
        cwd,
        env: { ...env, GENTS_LIVE_CONFIG: "1", GENTS_EVAL_RUN_DIR: directory },
        stdio: ["ignore", "pipe", "pipe"],
        detached: process.platform !== "win32",
      },
    );
    let requestedSignal = null;
    let killTimer;
    const signalGroup = (signal) => {
      if (!child.pid) return;
      try {
        if (process.platform === "win32") child.kill(signal);
        else process.kill(-child.pid, signal);
      } catch (error) {
        if (error.code !== "ESRCH") execution.error = error.message;
      }
    };
    const stop = (signal) => {
      if (requestedSignal) return signalGroup("SIGKILL");
      requestedSignal = signal;
      signalGroup(signal);
      killTimer = setTimeout(() => signalGroup("SIGKILL"), 10_000);
      killTimer.unref();
    };
    const interrupt = () => stop("SIGINT");
    const terminate = () => stop("SIGTERM");
    process.on("SIGINT", interrupt);
    process.on("SIGTERM", terminate);
    log.on("error", (error) => {
      execution.error = `Cannot retain runner log: ${error.message}`;
      stop("SIGTERM");
    });
    child.stdout.on("data", (chunk) => {
      log.write(chunk);
    });
    child.stderr.on("data", (chunk) => {
      log.write(chunk);
    });
    const outcome = await new Promise((done) => {
      child.on("error", (error) => {
        execution.error = error.message;
      });
      child.on("close", (code, signal) => done({ code, signal }));
    });
    process.off("SIGINT", interrupt);
    process.off("SIGTERM", terminate);
    clearTimeout(killTimer);
    log.end();
    await finished(log).catch((error) => {
      execution.error ??= error.message;
    });
    execution.finished_at = new Date().toISOString();
    execution.exit_code = outcome.code;
    execution.signal = requestedSignal || outcome.signal;
    if (outcome.code === 0 && !execution.signal && !execution.error) {
      try {
        const report = JSON.parse(
          await readFile(join(directory, "report.json"), "utf8"),
        );
        if (
          report.schema_version !== 1 ||
          report.status !== "completed" ||
          !(report.planned > 0) ||
          report.completed !== report.planned ||
          report.failed !== 0
        ) {
          throw new Error(
            "Cargo succeeded without a complete passing eval report",
          );
        }
      } catch (error) {
        execution.error = `Cannot establish successful eval: ${error.message}`;
      }
    }
    execution.status = execution.signal
      ? "interrupted"
      : execution.error
        ? "error"
        : "exited";
    await save();
    await stopDashboard();
    stdout.write(await renderReport(directory));
    return {
      directory,
      exitCode: execution.signal
        ? 128 + (constants.signals[execution.signal] || 1)
        : execution.error
          ? 1
          : (outcome.code ?? 1),
    };
  } finally {
    await stopDashboard();
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  try {
    const cargo = process.argv.slice(2);
    process.exitCode = (
      await runConfigurator({ cargo: cargo.length ? cargo : ["cargo"] })
    ).exitCode;
  } catch (error) {
    process.stderr.write(`${error.stack}\n`);
    process.exitCode = 1;
  }
}
