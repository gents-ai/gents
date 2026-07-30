import type { ChildProcessWithoutNullStreams } from "node:child_process";

import type { RunnerReadyMessage } from "./types";

export async function waitForReadyMessage(
  process: ChildProcessWithoutNullStreams,
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
          `bridge runner did not become ready within ${timeoutMs}ms\nstdout:\n${stdout}\nstderr:\n${stderr}`,
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

    const onExit = (code: number | null) => {
      cleanup();
      reject(
        new Error(
          `bridge runner exited before ready (code=${code ?? "null"})\nstdout:\n${stdout}${stdoutBuffer}\nstderr:\n${stderr}`,
        ),
      );
    };

    const cleanup = () => {
      clearTimeout(timeout);
      process.stdout.off("data", onStdout);
      process.stderr.off("data", onStderr);
      process.off("exit", onExit);
    };

    process.stdout.on("data", onStdout);
    process.stderr.on("data", onStderr);
    process.on("exit", onExit);
  });
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
