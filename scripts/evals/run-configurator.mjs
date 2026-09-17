import { spawn, execFileSync } from "node:child_process";
import { openSync, closeSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rename, writeFile } from "node:fs/promises";
import { constants, homedir } from "node:os";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { renderReport } from "./report.mjs";
import { startDashboard } from "./watch.mjs";
import { resolveRuntimeImage, runtimeMemoryPlan } from "./host-environment.mjs";

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
  const suite = env.GENTS_EVAL_SUITE || "progressive-configurator";
  if (
    !["progressive-configurator", "monitor-mailbox", "host-steward"].includes(
      suite,
    )
  ) {
    throw new Error(`Unsupported eval suite: ${suite}`);
  }
  await mkdir(root, { recursive: true, mode: 0o700 });
  const timestamp = new Date().toISOString().replace(/[:.]/g, "-");
  const directory = await mkdtemp(join(root, `${suite}-${timestamp}-`));
  const execution = {
    schema_version: 1,
    started_at: new Date().toISOString(),
    heartbeat_at: new Date().toISOString(),
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
  let heartbeatPending = Promise.resolve();
  let savingHeartbeat = false;
  const heartbeat = setInterval(() => {
    if (savingHeartbeat) return;
    savingHeartbeat = true;
    heartbeatPending = (async () => {
      execution.heartbeat_at = new Date().toISOString();
      await save();
    })()
      .catch((error) => {
        execution.error = `Cannot retain runner heartbeat: ${error.message}`;
      })
      .finally(() => {
        savingHeartbeat = false;
      });
  }, 5000);
  stdout.write(`Eval directory: ${directory}\n`);
  const stopDashboard = startDashboard(directory, { output: stdout });
  try {
    const hostEnvironment = {};
    if (suite === "host-steward") {
      const runtimeImage = await resolveRuntimeImage(
        env.GENTS_HOST_RUNTIME_IMAGE,
        execution.revision,
      );
      const memory = await runtimeMemoryPlan(
        Number(env.GENTS_LIVE_CONFIG_CONCURRENCY || 1),
      );
      hostEnvironment.GENTS_HOST_RUNTIME_IMAGE = runtimeImage;
      await writeFile(
        join(directory, "host-environment.json"),
        `${JSON.stringify({ runtime_image: runtimeImage, memory }, null, 2)}\n`,
        { mode: 0o600, flag: "wx" },
      );
      if (!memory.sufficient)
        throw new Error(
          `Host cohort needs at least ${memory.required_bytes} bytes of Docker VM memory including overhead; available limit is ${memory.vm_memory_bytes}. Increase the VM limit or explicitly choose a smaller concurrency.`,
        );
    }
    const log = openSync(join(directory, "runner.log"), "a", 0o600);
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
        env: {
          ...env,
          ...hostEnvironment,
          GENTS_LIVE_CONFIG: "1",
          GENTS_EVAL_RUN_DIR: directory,
          GENTS_EVAL_SOURCE_REVISION: execution.revision?.commit || "unknown",
          GENTS_EVAL_SOURCE_DIRTY: String(execution.revision?.dirty ?? true),
        },
        // Direct file descriptors survive a killed display/launcher; a pipe
        // would turn observer loss into broken-pipe panics in the runtime.
        stdio: ["ignore", log, log],
        detached: process.platform !== "win32",
      },
    );
    closeSync(log);
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
    const outcome = await new Promise((done) => {
      child.on("error", (error) => {
        execution.error = error.message;
      });
      child.on("close", (code, signal) => done({ code, signal }));
    });
    process.off("SIGINT", interrupt);
    process.off("SIGTERM", terminate);
    clearTimeout(killTimer);
    clearInterval(heartbeat);
    await heartbeatPending;
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
  } catch (error) {
    clearInterval(heartbeat);
    await heartbeatPending;
    execution.finished_at = new Date().toISOString();
    execution.status = "error";
    execution.error = error.message;
    await save();
    await stopDashboard();
    stdout.write(await renderReport(directory));
    return { directory, exitCode: 1 };
  } finally {
    clearInterval(heartbeat);
    await heartbeatPending;
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
