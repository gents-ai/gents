import type { ChildProcessWithoutNullStreams } from "node:child_process";

import type { RunnerReadyMessage } from "./types";

const RUNNER_STOP_GRACE_MS = 2_000;
const RUNNER_KILL_DRAIN_MS = 2_000;
const RUNNER_DISPOSE_GRACE_MS = 10_000;

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

    const cleanup = () => {
      clearTimeout(timeout);
      child.stdout.off("data", onStdout);
      child.stderr.off("data", onStderr);
      child.off("exit", onExit);
    };

    child.stdout.on("data", onStdout);
    child.stderr.on("data", onStderr);
    child.on("exit", onExit);
  });
}

export async function terminateRunnerProcess(
  child: ChildProcessWithoutNullStreams,
  {
    graceMs = RUNNER_STOP_GRACE_MS,
    killDrainMs = RUNNER_KILL_DRAIN_MS,
  }: { graceMs?: number; killDrainMs?: number } = {},
) {
  endRunnerStdin(child);
  child.stdout.resume();
  child.stderr.resume();

  const processGroupId = runnerProcessGroupId(child);
  signalRunnerProcess(child, "SIGTERM", processGroupId);
  if (await waitForRunnerDrain(child, processGroupId, graceMs)) {
    return;
  }

  signalRunnerProcess(child, "SIGKILL", processGroupId);
  if (!(await waitForRunnerDrain(child, processGroupId, killDrainMs))) {
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
  if (await waitForRunnerDrain(child, runnerProcessGroupId(child), gracefulExitMs)) {
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
) {
  if (processGroupId !== null) {
    try {
      process.kill(-processGroupId, signal);
      return;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ESRCH") {
        throw error;
      }
    }
  }

  if (child.exitCode === null && child.signalCode === null) {
    child.kill(signal);
  }
}

async function waitForRunnerDrain(
  child: ChildProcessWithoutNullStreams,
  processGroupId: number | null,
  timeoutMs: number,
) {
  const [childExited, processGroupExited] = await Promise.all([
    waitForChildExit(child, timeoutMs),
    processGroupId === null
      ? Promise.resolve(true)
      : waitForProcessGroupExit(processGroupId, timeoutMs),
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

async function waitForProcessGroupExit(processGroupId: number, timeoutMs: number) {
  const deadline = Date.now() + timeoutMs;
  while (processGroupExists(processGroupId)) {
    const remainingMs = deadline - Date.now();
    if (remainingMs <= 0) {
      return false;
    }
    await new Promise((resolve) => setTimeout(resolve, Math.min(25, remainingMs)));
  }
  return true;
}

function processGroupExists(processGroupId: number) {
  try {
    process.kill(-processGroupId, 0);
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
