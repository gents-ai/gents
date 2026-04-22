import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { resolve } from "node:path";

import type { DesktopApiAdapter } from "../src/lib/desktop-api";
import type { DesktopClientUpdatedListenerFactory } from "../src/lib/desktop-events";
import type {
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  InitSummary,
} from "../src/lib/types";
import type {
  TauriDriverBridge,
  TauriDriverChatRequest,
} from "./tauri-driver";

type RunnerReadyMessage = {
  kind: "ready";
  baseUrl: string;
  deploymentLabel: string;
  agentDid: string;
};

type VersionResponse = {
  version: number;
};

type ToolCallDiagnostics = {
  total: number;
  completed: number;
  pending: number;
  latestToolName?: string | null;
  latestStatus?: string | null;
  latestCompletedAt?: string | null;
};

export type RequestDiagnostics = {
  source: string;
  sessionId: string;
  requestId: string;
  turnState?: string | null;
  latestRequestId?: string | null;
  conversationUpdatedAt?: string | null;
  request?: {
    status?: string | null;
    lifecycleState?: string | null;
    failureReason?: string | null;
    createdAt?: string | null;
    claimedAt?: string | null;
    interruptRequestedAt?: string | null;
    validUntil?: string | null;
  } | null;
  response?: {
    status?: string | null;
    errorMessage?: string | null;
    progressSeq?: number | null;
    materializedMessageSequence?: number | null;
    materializedAt?: string | null;
    completedAt?: string | null;
    contentLen: number;
    reasoningLen: number;
  } | null;
  toolCalls: ToolCallDiagnostics;
  toolResultCount: number;
  messageCount: number;
  timelineCount: number;
  activeResponseOverlayContentLen: number;
  activeResponseOverlayReasoningLen: number;
};

export type RequestDiagnosticsBundle = {
  desktop: RequestDiagnostics;
  remote: RequestDiagnostics;
};

export type RemoteTerminalDesktopStallObservation = {
  startedAt: number | null;
  stallMs: number | null;
  exceededThreshold: boolean;
};

export type RemoteAheadDesktopLagObservation = {
  startedAt: number | null;
  lagMs: number | null;
  exceededThreshold: boolean;
};

type LiveBridgeRunnerOptions = {
  inferenceUrl?: string | null;
  modelName?: string | null;
  provider?: string | null;
  apiKey?: string | null;
  apiKeyEnvVar?: string | null;
};

const RUNNER_START_TIMEOUT_MS = 120_000;
const REQUEST_TIMEOUT_MS = 600_000;
const HTTP_REQUEST_TIMEOUT_MS = 15_000;
const VERSION_POLL_MS = 250;
const REMOTE_TERMINAL_DESKTOP_STALL_MS = 30_000;
const REMOTE_AHEAD_DESKTOP_LAG_MS = 30_000;
const REPO_ROOT = resolve(process.cwd(), "../..");

function isTerminalTurnState(value?: string | null) {
  return (
    value === "completed" ||
    value === "failed" ||
    value === "superseded" ||
    value === "interrupted"
  );
}

export function observeRemoteTerminalDesktopStall({
  diagnostics,
  previousStartedAt,
  now,
  thresholdMs = REMOTE_TERMINAL_DESKTOP_STALL_MS,
}: {
  diagnostics: RequestDiagnosticsBundle;
  previousStartedAt: number | null;
  now: number;
  thresholdMs?: number;
}): RemoteTerminalDesktopStallObservation {
  if (
    !isTerminalTurnState(diagnostics.remote.turnState) ||
    isTerminalTurnState(diagnostics.desktop.turnState)
  ) {
    return {
      startedAt: null,
      stallMs: null,
      exceededThreshold: false,
    };
  }

  const startedAt = previousStartedAt ?? now;
  const stallMs = now - startedAt;
  return {
    startedAt,
    stallMs,
    exceededThreshold:
      previousStartedAt !== null && stallMs >= thresholdMs,
  };
}

function progressNumber(value?: number | null) {
  return value ?? 0;
}

function requestProgressSignature(diagnostics: RequestDiagnostics) {
  return JSON.stringify({
    turnState: diagnostics.turnState ?? null,
    latestRequestId: diagnostics.latestRequestId ?? null,
    requestStatus: diagnostics.request?.status ?? null,
    requestLifecycleState: diagnostics.request?.lifecycleState ?? null,
    responseStatus: diagnostics.response?.status ?? null,
    responseProgressSeq: progressNumber(diagnostics.response?.progressSeq),
    materializedMessageSequence: progressNumber(
      diagnostics.response?.materializedMessageSequence,
    ),
    responseContentLen: progressNumber(diagnostics.response?.contentLen),
    responseReasoningLen: progressNumber(diagnostics.response?.reasoningLen),
    toolCallsCompleted: diagnostics.toolCalls.completed,
    toolCallsPending: diagnostics.toolCalls.pending,
    toolResultCount: diagnostics.toolResultCount,
    messageCount: diagnostics.messageCount,
    timelineCount: diagnostics.timelineCount,
    activeResponseOverlayContentLen: diagnostics.activeResponseOverlayContentLen,
    activeResponseOverlayReasoningLen: diagnostics.activeResponseOverlayReasoningLen,
  });
}

function isRemoteAheadOfDesktop(diagnostics: RequestDiagnosticsBundle) {
  return (
    progressNumber(diagnostics.remote.response?.progressSeq) >
      progressNumber(diagnostics.desktop.response?.progressSeq) ||
    progressNumber(diagnostics.remote.response?.materializedMessageSequence) >
      progressNumber(diagnostics.desktop.response?.materializedMessageSequence) ||
    progressNumber(diagnostics.remote.response?.contentLen) >
      progressNumber(diagnostics.desktop.response?.contentLen) ||
    diagnostics.remote.toolCalls.completed > diagnostics.desktop.toolCalls.completed ||
    diagnostics.remote.toolResultCount > diagnostics.desktop.toolResultCount ||
    diagnostics.remote.messageCount > diagnostics.desktop.messageCount ||
    diagnostics.remote.timelineCount > diagnostics.desktop.timelineCount ||
    diagnostics.remote.activeResponseOverlayContentLen >
      diagnostics.desktop.activeResponseOverlayContentLen
  );
}

export function observeRemoteAheadDesktopLag({
  diagnostics,
  desktopProgressed,
  previousStartedAt,
  now,
  thresholdMs = REMOTE_AHEAD_DESKTOP_LAG_MS,
}: {
  diagnostics: RequestDiagnosticsBundle;
  desktopProgressed: boolean;
  previousStartedAt: number | null;
  now: number;
  thresholdMs?: number;
}): RemoteAheadDesktopLagObservation {
  if (
    desktopProgressed ||
    isTerminalTurnState(diagnostics.desktop.turnState) ||
    !isRemoteAheadOfDesktop(diagnostics)
  ) {
    return {
      startedAt: null,
      lagMs: null,
      exceededThreshold: false,
    };
  }

  const startedAt = previousStartedAt ?? now;
  const lagMs = now - startedAt;
  return {
    startedAt,
    lagMs,
    exceededThreshold:
      previousStartedAt !== null && lagMs >= thresholdMs,
  };
}

async function waitForLine(
  process: ChildProcessWithoutNullStreams,
  timeoutMs: number,
) {
  let stdout = "";
  let stderr = "";

  return await new Promise<string>((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup();
      reject(
        new Error(
          `bridge runner did not become ready within ${timeoutMs}ms\nstderr:\n${stderr}`,
        ),
      );
    }, timeoutMs);

    const onStdout = (chunk: Buffer) => {
      stdout += chunk.toString();
      const newlineIndex = stdout.indexOf("\n");
      if (newlineIndex === -1) {
        return;
      }
      const line = stdout.slice(0, newlineIndex).trim();
      cleanup();
      resolve(line);
    };

    const onStderr = (chunk: Buffer) => {
      stderr += chunk.toString();
    };

    const onExit = (code: number | null) => {
      cleanup();
      reject(
        new Error(
          `bridge runner exited before ready (code=${code ?? "null"})\nstderr:\n${stderr}`,
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

export class LiveBridgeRunner implements TauriDriverBridge {
  readonly sentRequests: TauriDriverChatRequest[] = [];
  readonly sendResults: ChatSendResult[] = [];
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
    ) {
    this.process.stderr.on("data", (chunk: Buffer) => {
      this.pushLogChunk(this.stderrChunks, chunk.toString());
    });
    this.process.stdout.on("data", (chunk: Buffer) => {
      this.pushLogChunk(this.stdoutChunks, chunk.toString());
    });
    this.process.once("exit", (code, signal) => {
      this.exitStatus = { code, signal };
    });
    this.adapter = {
      fetchDesktopSnapshot: async () =>
        this.getJson<DesktopClientSnapshot>("/desktop/client/snapshot"),
      initLocalStandardRuntime: async () =>
        this.postJson<InitSummary>("/desktop/init", {}),
      startDesktopClient: async () =>
        this.postJson<DesktopClientSnapshot>("/desktop/client/start", {}),
      shutdownDesktopClient: async () =>
        this.postJson<DesktopClientSnapshot>("/desktop/client/shutdown", {}),
      fetchSessionSnapshot: async (sessionId, requestId) =>
        this.postJson<DesktopSessionSnapshot | null>(
          "/desktop/session/snapshot",
          {
            sessionId,
            requestId: requestId ?? null,
          },
        ),
      sendChatMessage: async (request) => {
        const normalized = {
          agentDid: request.agentDid,
          behaviorId: request.behaviorId ?? null,
          sessionId: request.sessionId ?? null,
          content: request.content,
        };
        this.sentRequests.push(normalized);
        const result = await this.postJson<ChatSendResult>(
          "/desktop/chat/send",
          normalized,
        );
        this.sendResults.push(result);
        return result;
      },
      renameConversation: async (request) => {
        await this.postJson("/desktop/conversation/rename", request);
      },
    };
    this.listenerFactory = async (handler) => {
      let disposed = false;
      let inFlight = false;
      let lastVersion = await this.fetchVersion();
      const timer = setInterval(async () => {
        if (disposed || inFlight) {
          return;
        }
        inFlight = true;
        try {
          const nextVersion = await this.fetchVersion();
          if (nextVersion !== lastVersion) {
            lastVersion = nextVersion;
            await handler();
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
    const child = spawn(
      "cargo",
      runnerArgs,
      {
        cwd: REPO_ROOT,
        env: process.env,
        stdio: ["pipe", "pipe", "pipe"],
      },
    );

    const line = await waitForLine(child, RUNNER_START_TIMEOUT_MS);
    const message = JSON.parse(line) as RunnerReadyMessage;
    if (message.kind !== "ready") {
      throw new Error(`unexpected bridge runner ready payload: ${line}`);
    }
    return new LiveBridgeRunner(
      child,
      message.baseUrl,
      message.deploymentLabel,
      message.agentDid,
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
        const desktopProgressSignature = requestProgressSignature(
          diagnostics.desktop,
        );
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
        remoteTerminalDesktopStallStartedAt =
          remoteTerminalDesktopStall.startedAt;
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

  private async getJson<T>(path: string) {
    const response = await this.fetchWithTimeout(`${this.baseUrl}${path}`, {});
    return this.decodeJson<T>(response);
  }

  private async postJson<T = unknown>(path: string, body: unknown) {
    const response = await this.fetchWithTimeout(`${this.baseUrl}${path}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
    });
    return this.decodeJson<T>(response);
  }

  private async fetchRequestDiagnostics(
    sessionId: string,
    requestId: string,
  ) {
    return await this.postJson<RequestDiagnosticsBundle>(
      "/desktop/request/diagnostics",
      {
        sessionId,
        requestId,
      },
    );
  }

  private async fetchWithTimeout(input: string, init: RequestInit) {
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

  private async decodeJson<T>(response: Response) {
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

function appendRunnerArg(
  args: string[],
  flag: string,
  value?: string | null,
) {
  const trimmed = value?.trim();
  if (!trimmed) {
    return;
  }
  args.push(flag, trimmed);
}
