import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { resolve } from "node:path";

import type { DesktopApiAdapter } from "../src/lib/desktop-api";
import type { DesktopClientUpdatedListenerFactory } from "../src/lib/desktop-events";
import type {
  ChatSendResult,
  DesktopClientSnapshot,
  TaskRunResult,
} from "../src/lib/types";
import type { TauriDriverBridge, TauriDriverChatRequest } from "./tauri-driver";
import { createRunnerAdapter } from "./live-bridge-runner/adapter";
import { JsonHttpClient } from "./live-bridge-runner/http";
import {
  createVersionPollingListenerFactory,
  VERSION_POLL_MS,
} from "./live-bridge-runner/listener";
import { RunnerLogs, type RunnerExitStatus } from "./live-bridge-runner/logs";
import { appendRunnerArg, waitForReadyMessage } from "./live-bridge-runner/process";
import {
  waitForRequestCompletion,
  type RequestCompletionTarget,
} from "./live-bridge-runner/request-completion";
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
const REPO_ROOT = resolve(process.cwd(), "../..");

export class LiveBridgeRunner implements TauriDriverBridge {
  readonly sentRequests: TauriDriverChatRequest[] = [];
  readonly sendResults: ChatSendResult[] = [];
  readonly taskRunResults: TaskRunResult[] = [];
  readonly adapter: DesktopApiAdapter;
  readonly listenerFactory: DesktopClientUpdatedListenerFactory;
  private readonly http: JsonHttpClient;
  private readonly logs = new RunnerLogs();
  private exitStatus: RunnerExitStatus | null = null;

  private constructor(
    private readonly process: ChildProcessWithoutNullStreams,
    readonly baseUrl: string,
    readonly deploymentLabel: string,
    readonly agentDid: string,
    readonly toolRoot: string,
    startupStdout = "",
    startupStderr = "",
  ) {
    this.http = new JsonHttpClient(baseUrl);
    this.logs.pushStdout(startupStdout);
    this.logs.pushStderr(startupStderr);
    this.process.stderr.on("data", (chunk: Buffer) => {
      this.logs.pushStderr(chunk.toString());
    });
    this.process.stdout.on("data", (chunk: Buffer) => {
      this.logs.pushStdout(chunk.toString());
    });
    this.process.once("exit", (code, signal) => {
      this.exitStatus = { code, signal };
    });
    this.adapter = createRunnerAdapter(this);
    this.listenerFactory = createVersionPollingListenerFactory({
      fetchVersion: () => this.fetchVersion(),
      getExitStatus: () => this.exitStatus,
      logError: (message) => this.logs.pushStderr(message),
      pollMs: VERSION_POLL_MS,
    });
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
    request: RequestCompletionTarget,
    timeoutMs = REQUEST_TIMEOUT_MS,
  ) {
    return waitForRequestCompletion({
      request,
      adapter: this.adapter,
      fetchRequestDiagnostics: (sessionId, requestId) =>
        this.fetchRequestDiagnostics(sessionId, requestId),
      getExitStatus: () => this.exitStatus,
      stdoutTail: () => this.logs.stdoutTail(),
      stderrTail: () => this.logs.stderrTail(),
      timeoutMs,
    });
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

  async getJson<T>(path: string) {
    return this.http.getJson<T>(path);
  }

  async postJson<T = unknown>(path: string, body: unknown) {
    return this.http.postJson<T>(path, body);
  }

  async fetchWithTimeout(input: string, init: RequestInit) {
    return this.http.fetchWithTimeout(input, init);
  }

  async decodeJson<T>(response: Response) {
    return this.http.decodeJson<T>(response);
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

  private async fetchVersion() {
    const response = await this.getJson<VersionResponse>("/desktop/version");
    return response.version;
  }
}
