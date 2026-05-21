import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { resolve } from "node:path";

import type { DesktopApiAdapter } from "../src/lib/desktop-api";
import type { DesktopClientUpdatedListenerFactory } from "../src/lib/desktop-events";
import type {
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  TaskRunResult,
} from "../src/lib/types";
import type { TauriDriverBridge, TauriDriverChatRequest } from "./tauri-driver";
import {
  isTerminalTurnState,
  observeRemoteAheadDesktopLag,
  observeRemoteTerminalDesktopStall,
  requestProgressSignature,
} from "./live-bridge-runner/observations";
import { createRunnerAdapter } from "./live-bridge-runner/adapter";
import { appendRunnerArg, waitForReadyMessage } from "./live-bridge-runner/process";
import type {
  LiveBridgeRunnerOptions,
  RequestDiagnosticsBundle,
  VersionResponse,
} from "./live-bridge-runner/types";

export {
  observeRemoteAheadDesktopLag,
  observeRemoteTerminalDesktopStall,
} from "./live-bridge-runner/observations";
export type {
  LiveBridgeRunnerOptions,
  RemoteAheadDesktopLagObservation,
  RemoteTerminalDesktopStallObservation,
  RequestDiagnostics,
  RequestDiagnosticsBundle,
} from "./live-bridge-runner/types";

const RUNNER_START_TIMEOUT_MS = 300_000;
const REQUEST_TIMEOUT_MS = 600_000;
const HTTP_REQUEST_TIMEOUT_MS = 15_000;
const VERSION_POLL_MS = 250;
const REPO_ROOT = resolve(process.cwd(), "../..");

export class LiveBridgeRunner implements TauriDriverBridge {
  readonly sentRequests: TauriDriverChatRequest[] = [];
  readonly sendResults: ChatSendResult[] = [];
  readonly taskRunResults: TaskRunResult[] = [];
  readonly adapter: DesktopApiAdapter;
  readonly listenerFactory: DesktopClientUpdatedListenerFactory;
  private readonly stderrChunks: string[] = [];
  private readonly stdoutChunks: string[] = [];
  private exitStatus: { code: number | null; signal: NodeJS.Signals | null } | null =
    null;

  private constructor(
    private readonly process: ChildProcessWithoutNullStreams,
    readonly baseUrl: string,
    readonly deploymentLabel: string,
    readonly agentDid: string,
    readonly toolRoot: string,
    startupStdout = "",
    startupStderr = "",
  ) {
    this.pushLogChunk(this.stdoutChunks, startupStdout);
    this.pushLogChunk(this.stderrChunks, startupStderr);
    this.process.stderr.on("data", (chunk: Buffer) => {
      this.pushLogChunk(this.stderrChunks, chunk.toString());
    });
    this.process.stdout.on("data", (chunk: Buffer) => {
      this.pushLogChunk(this.stdoutChunks, chunk.toString());
    });
    this.process.once("exit", (code, signal) => {
      this.exitStatus = { code, signal };
    });
    this.adapter = createRunnerAdapter(this);
    this.listenerFactory = async (handler) => {
      let disposed = false;
      let inFlight = false;
      let lastVersion = 0;
      try {
        lastVersion = await this.fetchVersion();
      } catch (error) {
        if (!this.exitStatus) {
          this.pushLogChunk(this.stderrChunks, `[listener:init] ${String(error)}\n`);
        }
      }
      const timer = setInterval(async () => {
        if (disposed || inFlight) {
          return;
        }
        inFlight = true;
        try {
          const nextVersion = await this.fetchVersion();
          if (nextVersion !== lastVersion) {
            lastVersion = nextVersion;
            await handler({ reason: "store" });
          }
        } catch (error) {
          if (!disposed && !this.exitStatus) {
            this.pushLogChunk(this.stderrChunks, `[listener] ${String(error)}\n`);
          }
        } finally {
          inFlight = false;
        }
      }, VERSION_POLL_MS);

      return () => {
        disposed = true;
        clearInterval(timer);
      };
    };
  }

  static async start(options: LiveBridgeRunnerOptions = {}) {
    const runnerArgs = [
      "run",
      "-p",
      "defra-agent-desktop-tauri",
      "--bin",
      "bridge_runner",
      "--quiet",
      "--",
    ];
    appendRunnerArg(runnerArgs, "--inference-url", options.inferenceUrl);
    appendRunnerArg(runnerArgs, "--model-name", options.modelName);
    appendRunnerArg(runnerArgs, "--provider", options.provider);
    appendRunnerArg(runnerArgs, "--api-key", options.apiKey);
    appendRunnerArg(runnerArgs, "--api-key-env-var", options.apiKeyEnvVar);
    const child = spawn("cargo", runnerArgs, {
      cwd: REPO_ROOT,
      env: {
        ...process.env,
        CARGO_NET_GIT_FETCH_WITH_CLI:
          process.env.CARGO_NET_GIT_FETCH_WITH_CLI ?? "true",
      },
      stdio: ["pipe", "pipe", "pipe"],
    });

    const { message, stdout, stderr } = await waitForReadyMessage(
      child,
      RUNNER_START_TIMEOUT_MS,
    );
    return new LiveBridgeRunner(
      child,
      message.baseUrl,
      message.deploymentLabel,
      message.agentDid,
      message.toolRoot,
      stdout,
      stderr,
    );
  }

  async fetchSnapshot() {
    return this.adapter.fetchDesktopSnapshot();
  }

  async waitForRequestCompletion(
    request: ChatSendResult,
    timeoutMs = REQUEST_TIMEOUT_MS,
  ) {
    const deadline = Date.now() + timeoutMs;
    let lastObservedState = "no diagnostics observed yet";
    let lastError: string | null = null;
    const progressHistory: string[] = [];
    let lastProgressSignature = "";
    let lastDesktopProgressSignature = "";
    let remoteTerminalDesktopStallStartedAt: number | null = null;
    let remoteAheadDesktopLagStartedAt: number | null = null;
    while (Date.now() < deadline) {
      this.throwIfExited(
        `waiting for request ${request.requestId} to complete`,
        lastObservedState,
        lastError,
        progressHistory,
      );
      try {
        const diagnostics = await this.fetchRequestDiagnostics(
          request.sessionId,
          request.requestId,
        );
        lastError = null;
        const progressSignature = JSON.stringify({
          desktop: diagnostics.desktop,
          remote: diagnostics.remote,
        });
        const desktopProgressSignature = requestProgressSignature(diagnostics.desktop);
        const desktopProgressed =
          desktopProgressSignature !== lastDesktopProgressSignature;
        if (desktopProgressed) {
          lastDesktopProgressSignature = desktopProgressSignature;
        }
        if (progressSignature !== lastProgressSignature) {
          lastProgressSignature = progressSignature;
          lastObservedState = progressSignature;
          progressHistory.push(progressSignature);
          if (progressHistory.length > 8) {
            progressHistory.shift();
          }
        }
        const remoteTerminalDesktopStall = observeRemoteTerminalDesktopStall({
          diagnostics,
          previousStartedAt: remoteTerminalDesktopStallStartedAt,
          now: Date.now(),
        });
        remoteTerminalDesktopStallStartedAt = remoteTerminalDesktopStall.startedAt;
        if (remoteTerminalDesktopStall.exceededThreshold) {
          throw new Error(
            `desktop stalled after remote terminal response for request ${request.requestId}; stallMs=${remoteTerminalDesktopStall.stallMs ?? 0}; diagnostics=${JSON.stringify({ desktop: diagnostics.desktop, remote: diagnostics.remote })}; runnerStdoutTail=${JSON.stringify(this.logTail(this.stdoutChunks))}; runnerStderrTail=${JSON.stringify(this.logTail(this.stderrChunks))}`,
          );
        }
        const remoteAheadDesktopLag = observeRemoteAheadDesktopLag({
          diagnostics,
          desktopProgressed,
          previousStartedAt: remoteAheadDesktopLagStartedAt,
          now: Date.now(),
        });
        remoteAheadDesktopLagStartedAt = remoteAheadDesktopLag.startedAt;
        if (remoteAheadDesktopLag.exceededThreshold) {
          throw new Error(
            `desktop stopped materializing progress while remote advanced for request ${request.requestId}; lagMs=${remoteAheadDesktopLag.lagMs ?? 0}; diagnostics=${JSON.stringify({ desktop: diagnostics.desktop, remote: diagnostics.remote })}; runnerStdoutTail=${JSON.stringify(this.logTail(this.stdoutChunks))}; runnerStderrTail=${JSON.stringify(this.logTail(this.stderrChunks))}`,
          );
        }
        if (isTerminalTurnState(diagnostics.desktop.turnState)) {
          const snapshot = await this.adapter.fetchSessionSnapshot(
            request.sessionId,
            request.agentDid,
            request.requestId,
          );
          if (snapshot) {
            return snapshot;
          }
        }
      } catch (error) {
        lastError = String(error);
        this.throwIfExited(
          `waiting for request ${request.requestId} to complete`,
          lastObservedState,
          lastError,
          progressHistory,
        );
      }
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
    throw new Error(
      `timed out waiting for request ${request.requestId} to complete; lastObservedState=${lastObservedState}; lastError=${lastError ?? "none"}; progressHistory=${JSON.stringify(progressHistory)}; runnerStderrTail=${JSON.stringify(this.logTail(this.stderrChunks))}`,
    );
  }

  async dispose() {
    this.process.stdin.end();
    const exited = await new Promise<boolean>((resolve) => {
      const timeout = setTimeout(() => {
        this.process.kill("SIGKILL");
        resolve(false);
      }, 10_000);
      this.process.once("exit", () => {
        clearTimeout(timeout);
        resolve(true);
      });
    });
    if (!exited) {
      await new Promise((resolve) => this.process.once("exit", resolve));
    }
  }

  private async fetchVersion() {
    const response = await this.getJson<VersionResponse>("/desktop/version");
    return response.version;
  }

  private throwIfExited(
    context: string,
    lastObservedState: string,
    lastError: string | null,
    progressHistory: string[],
  ) {
    if (!this.exitStatus) {
      return;
    }

    throw new Error(
      `bridge runner exited while ${context}; code=${this.exitStatus.code ?? "null"}; signal=${this.exitStatus.signal ?? "null"}; lastObservedState=${lastObservedState}; lastError=${lastError ?? "none"}; progressHistory=${JSON.stringify(progressHistory)}; runnerStdoutTail=${JSON.stringify(this.logTail(this.stdoutChunks))}; runnerStderrTail=${JSON.stringify(this.logTail(this.stderrChunks))}`,
    );
  }

  async getJson<T>(path: string) {
    const response = await this.fetchWithTimeout(`${this.baseUrl}${path}`, {});
    return this.decodeJson<T>(response);
  }

  async postJson<T = unknown>(path: string, body: unknown) {
    const response = await this.fetchWithTimeout(`${this.baseUrl}${path}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
    });
    return this.decodeJson<T>(response);
  }

  async fetchRequestDiagnostics(sessionId: string, requestId: string) {
    return await this.postJson<RequestDiagnosticsBundle>(
      "/desktop/request/diagnostics",
      {
        sessionId,
        requestId,
      },
    );
  }

  async fetchWithTimeout(input: string, init: RequestInit) {
    let timeoutId: ReturnType<typeof setTimeout> | null = null;
    try {
      return await Promise.race([
        fetch(input, init),
        new Promise<Response>((_, reject) => {
          timeoutId = setTimeout(() => {
            reject(
              new Error(
                `timed out after ${HTTP_REQUEST_TIMEOUT_MS}ms waiting for ${input}`,
              ),
            );
          }, HTTP_REQUEST_TIMEOUT_MS);
        }),
      ]);
    } finally {
      if (timeoutId) {
        clearTimeout(timeoutId);
      }
    }
  }

  async decodeJson<T>(response: Response) {
    if (!response.ok) {
      throw new Error(await response.text());
    }
    return (await response.json()) as T;
  }

  private pushLogChunk(chunks: string[], chunk: string) {
    chunks.push(chunk);
    while (chunks.join("").length > 8000) {
      chunks.shift();
    }
  }

  private logTail(chunks: string[]) {
    return chunks.join("").slice(-4000);
  }
}
