import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { constants, accessSync, statSync } from "node:fs";
import { isAbsolute, resolve } from "node:path";

import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { DesktopClientUpdatedListenerFactory } from "@source-inc/gents-desktop-client";
import type { ChatSendResult, TaskRunResult } from "@source-inc/gents-desktop-client";
import type { TauriDriverBridge, TauriDriverChatRequest } from "./tauri-driver";
import { createRunnerAdapter } from "./live-bridge-runner/adapter";
import { JsonHttpClient } from "./live-bridge-runner/http";
import {
  createVersionPollingListenerFactory,
  VERSION_POLL_MS,
} from "./live-bridge-runner/listener";
import { RunnerLogs, type RunnerExitStatus } from "./live-bridge-runner/logs";
import {
  appendRunnerArg,
  assertLiveBridgeRunnerPlatform,
  disposeRunnerProcess,
  waitForReadyMessage,
} from "./live-bridge-runner/process";
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
export const LIVE_RUNNER_BINARY_ENV = "GENTS_TAURI_LIVE_RUNNER_BINARY";

/**
 * Resolve the live runner process. A prebuilt override avoids Cargo's build lock,
 * but its caller owns ensuring that the explicitly selected binary is fresh.
 */
export function createLiveBridgeRunnerInvocation(
  options: LiveBridgeRunnerOptions = {},
  env: NodeJS.ProcessEnv = process.env,
) {
  const configuredBinary = env[LIVE_RUNNER_BINARY_ENV]?.trim();
  let command = "cargo";
  let runnerArgs = [
    "run",
    "-p",
    "gents-desktop-tauri",
    "--bin",
    "bridge_runner",
    "--quiet",
    "--",
  ];
  if (configuredBinary) {
    if (!isAbsolute(configuredBinary))
      throw new Error(`${LIVE_RUNNER_BINARY_ENV} must be an absolute path`);
    try {
      if (!statSync(configuredBinary).isFile()) throw new Error("not a file");
      accessSync(configuredBinary, constants.X_OK);
    } catch (error) {
      throw new Error(
        `${LIVE_RUNNER_BINARY_ENV} must name an executable file: ${configuredBinary}`,
        { cause: error },
      );
    }
    command = configuredBinary;
    runnerArgs = [];
  }
  appendRunnerArg(runnerArgs, "--inference-url", options.inferenceUrl);
  appendRunnerArg(runnerArgs, "--model-name", options.modelName);
  appendRunnerArg(runnerArgs, "--provider", options.provider);
  appendRunnerArg(runnerArgs, "--api-key", options.apiKey);
  appendRunnerArg(runnerArgs, "--api-key-env-var", options.apiKeyEnvVar);
  appendRunnerArg(runnerArgs, "--subagent-inference-url", options.subagentInferenceUrl);
  appendRunnerArg(runnerArgs, "--subagent-model-name", options.subagentModelName);
  appendRunnerArg(runnerArgs, "--subagent-provider", options.subagentProvider);
  appendRunnerArg(runnerArgs, "--subagent-api-key", options.subagentApiKey);
  appendRunnerArg(
    runnerArgs,
    "--subagent-api-key-env-var",
    options.subagentApiKeyEnvVar,
  );
  return { command, runnerArgs };
}

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
    assertLiveBridgeRunnerPlatform();
    const { command, runnerArgs } = createLiveBridgeRunnerInvocation(options);
    const child = spawn(command, runnerArgs, {
      cwd: REPO_ROOT,
      // Detachment makes bounded process-group cleanup possible. An abrupt
      // SIGKILL of this Node harness can still strand the group; the enclosing
      // test job remains the final reaping boundary for that case.
      detached: true,
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
    await disposeRunnerProcess(this.process);
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
