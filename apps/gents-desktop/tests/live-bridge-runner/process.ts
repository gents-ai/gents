import type { ChildProcessWithoutNullStreams } from "node:child_process";

import type { RunnerReadyMessage } from "./types";

const RUNNER_STOP_GRACE_MS = 2_000;
const RUNNER_KILL_DRAIN_MS = 2_000;
const RUNNER_DISPOSE_GRACE_MS = 10_000;

type KillByPid = (pid: number, signal: NodeJS.Signals | 0) => boolean;

const killByPid: KillByPid = (pid, signal) => process.kill(pid, signal);

export function assertLiveBridgeRunnerPlatform(platform = process.platform) {
  if (platform === "win32") {
    throw new Error(
      "the live bridge runner requires POSIX process-group cleanup and is not supported on Windows",
    );
  }
}

export async function waitForReadyMessage(
  child: ChildProcessWithoutNullStreams,
  timeoutMs: number,
) {
  try {
    return await readReadyMessage(child, timeoutMs);
  } catch (startupError) {
    try {
      await terminateRunnerProcess(child);
    } catch (cleanupError) {
      const startupMessage =
        startupError instanceof Error ? startupError.message : String(startupError);
      const cleanupMessage =
        cleanupError instanceof Error ? cleanupError.message : String(cleanupError);
      throw new Error(
        `${startupMessage}\nfailed to clean up bridge runner: ${cleanupMessage}`,
        { cause: cleanupError },
      );
    }
    throw startupError;
  }
}

async function readReadyMessage(
  child: ChildProcessWithoutNullStreams,
  timeoutMs: number,
) {
  let stdoutBuffer = "";
  let stdout = "";
  let stderr = "";

  return await new Promise<{
    message: RunnerReadyMessage;
    stdout: string;
    stderr: string;
  }>((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(
        new Error(
          `bridge runner did not become ready within ${timeoutMs}ms\nstdout:\n${stdout}${stdoutBuffer}\nstderr:\n${stderr}`,
        ),
      );
    }, timeoutMs);

    const tryResolveLine = (line: string) => {
      if (!line) {
        return;
      }
      stdout += `${line}\n`;
      try {
        const message = JSON.parse(line) as Partial<RunnerReadyMessage>;
        if (message.kind !== "ready") {
          return;
        }
        cleanup();
        resolve({
          message: message as RunnerReadyMessage,
          stdout,
          stderr,
        });
      } catch {}
    };

    const onStdout = (chunk: Buffer) => {
      stdoutBuffer += chunk.toString();
      let newlineIndex = stdoutBuffer.indexOf("\n");
      while (newlineIndex !== -1) {
        const line = stdoutBuffer.slice(0, newlineIndex).trim();
        stdoutBuffer = stdoutBuffer.slice(newlineIndex + 1);
        tryResolveLine(line);
        newlineIndex = stdoutBuffer.indexOf("\n");
      }
    };

    const onStderr = (chunk: Buffer) => {
      stderr += chunk.toString();
    };

    const onExit = (code: number | null, signal: NodeJS.Signals | null) => {
      cleanup();
      reject(
        new Error(
          `bridge runner exited before ready (code=${code ?? "null"}, signal=${signal ?? "null"})\nstdout:\n${stdout}${stdoutBuffer}\nstderr:\n${stderr}`,
        ),
      );
    };

    const onError = (error: Error) => {
      cleanup();
      reject(
        new Error(
          `bridge runner process error before ready: ${error.message}\nstdout:\n${stdout}${stdoutBuffer}\nstderr:\n${stderr}`,
          { cause: error },
        ),
      );
    };

    const cleanup = () => {
      clearTimeout(timeout);
      child.stdout.off("data", onStdout);
      child.stderr.off("data", onStderr);
      child.off("exit", onExit);
      child.off("error", onError);
    };

    child.stdout.on("data", onStdout);
    child.stderr.on("data", onStderr);
    child.on("exit", onExit);
    child.on("error", onError);
  });
}

export async function terminateRunnerProcess(
  child: ChildProcessWithoutNullStreams,
  {
    graceMs = RUNNER_STOP_GRACE_MS,
    killDrainMs = RUNNER_KILL_DRAIN_MS,
    killByPid: signalByPid = killByPid,
  }: { graceMs?: number; killDrainMs?: number; killByPid?: KillByPid } = {},
) {
  endRunnerStdin(child);
  child.stdout.resume();
  child.stderr.resume();

  if (!Number.isInteger(child.pid) || child.pid! <= 0) {
    return;
  }

  const processGroupId = runnerProcessGroupId(child);
  const termSignalSent = signalRunnerProcess(
    child,
    "SIGTERM",
    processGroupId,
    signalByPid,
  );
  if (!termSignalSent) {
    return;
  }
  if (await waitForRunnerDrain(child, processGroupId, graceMs, signalByPid)) {
    return;
  }

  const killSignalSent = signalRunnerProcess(
    child,
    "SIGKILL",
    processGroupId,
    signalByPid,
  );
  if (!killSignalSent) {
    return;
  }
  if (!(await waitForRunnerDrain(child, processGroupId, killDrainMs, signalByPid))) {
    throw new Error(
      `bridge runner process${processGroupId === null ? "" : ` group ${processGroupId}`} did not exit after SIGKILL`,
    );
  }
}

export async function disposeRunnerProcess(
  child: ChildProcessWithoutNullStreams,
  gracefulExitMs = RUNNER_DISPOSE_GRACE_MS,
) {
  endRunnerStdin(child);
  if (
    await waitForRunnerDrain(
      child,
      runnerProcessGroupId(child),
      gracefulExitMs,
      killByPid,
    )
  ) {
    return;
  }
  await terminateRunnerProcess(child);
}

function endRunnerStdin(child: ChildProcessWithoutNullStreams) {
  if (!child.stdin.destroyed && !child.stdin.writableEnded) {
    child.stdin.end();
  }
}

function runnerProcessGroupId(child: ChildProcessWithoutNullStreams) {
  return process.platform !== "win32" && Number.isInteger(child.pid) && child.pid! > 0
    ? child.pid!
    : null;
}

function signalRunnerProcess(
  child: ChildProcessWithoutNullStreams,
  signal: NodeJS.Signals,
  processGroupId: number | null,
  signalByPid: KillByPid,
) {
  if (processGroupId !== null) {
    try {
      signalByPid(-processGroupId, signal);
      return true;
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code;
      if (code === "EPERM" && childHasExited(child)) {
        // macOS can retain an unsignalable process-group identifier after the
        // leader has exited. Its successful exit remains authoritative.
        return false;
      }
      if (code !== "ESRCH") {
        throw error;
      }
    }
  }

  if (child.exitCode === null && child.signalCode === null) {
    child.kill(signal);
  }
  return true;
}

async function waitForRunnerDrain(
  child: ChildProcessWithoutNullStreams,
  processGroupId: number | null,
  timeoutMs: number,
  signalByPid: KillByPid,
) {
  const [childExited, processGroupExited] = await Promise.all([
    waitForChildExit(child, timeoutMs),
    processGroupId === null
      ? Promise.resolve(true)
      : waitForProcessGroupExit(processGroupId, timeoutMs, signalByPid),
  ]);
  return childExited && processGroupExited;
}

function waitForChildExit(child: ChildProcessWithoutNullStreams, timeoutMs: number) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve(true);
  }
  return new Promise<boolean>((resolve) => {
    const timeout = setTimeout(() => {
      child.off("exit", onExit);
      resolve(false);
    }, timeoutMs);
    const onExit = () => {
      clearTimeout(timeout);
      resolve(true);
    };
    child.once("exit", onExit);
  });
}

async function waitForProcessGroupExit(
  processGroupId: number,
  timeoutMs: number,
  signalByPid: KillByPid,
) {
  const deadline = Date.now() + timeoutMs;
  while (processGroupExists(processGroupId, signalByPid)) {
    const remainingMs = deadline - Date.now();
    if (remainingMs <= 0) {
      return false;
    }
    await new Promise((resolve) => setTimeout(resolve, Math.min(25, remainingMs)));
  }
  return true;
}

function processGroupExists(processGroupId: number, signalByPid: KillByPid) {
  try {
    signalByPid(-processGroupId, 0);
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ESRCH") {
      return false;
    }
    if ((error as NodeJS.ErrnoException).code === "EPERM") {
      return true;
    }
    throw error;
  }
}

function childHasExited(child: ChildProcessWithoutNullStreams) {
  return child.exitCode !== null || child.signalCode !== null;
}

export function appendRunnerArg(args: string[], flag: string, value?: string | null) {
  const trimmed = value?.trim();
  if (!trimmed) {
    return;
  }
  args.push(flag, trimmed);
}

export function normalizePeerStatusUrl(serverAddress: string) {
  const trimmed = serverAddress.trim();
  const url = new URL(
    trimmed.startsWith("http://") || trimmed.startsWith("https://")
      ? trimmed
      : `http://${trimmed}`,
  );
  const path = url.pathname.replace(/\/+$/, "");
  if (path === "" || path === "/" || path === "/api/v0" || path === "/api/v0/graphql") {
    url.pathname = "/status";
  } else if (!path.endsWith("/status")) {
    url.pathname = `${path}/status`;
  }
  url.search = "";
  url.hash = "";
  return url.toString();
}
