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
 * Resolve the live runner process. A prebuilt override avoids Cargo's build
 * lock; its caller owns ensuring that the binary is fresh. The runner takes
 * only flags; the harness's environment variables become flags here.
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
  if (enabled(env.GENTS_TAURI_LIVE)) runnerArgs.push("--live");
  if (enabled(env.GENTS_TAURI_LIVE_RETAIN_DATA)) runnerArgs.push("--retain-data");
  appendRunnerArg(runnerArgs, "--log-filter", env.GENTS_DESKTOP_TEST_LOG);
  appendRunnerArg(runnerArgs, "--inference-settings", env.GENTS_DESKTOP_LIVE_INFERENCE);
  appendRunnerArg(
    runnerArgs,
    "--inference-url",
    options.inferenceUrl ?? env.GENTS_DESKTOP_LIVE_BACKEND_ENDPOINT,
  );
  appendRunnerArg(
    runnerArgs,
    "--model-name",
    options.modelName ?? env.GENTS_DESKTOP_LIVE_BACKEND_MODEL,
  );
  appendRunnerArg(runnerArgs, "--fallback-model-name", env.GENTS_TEST_OPENROUTER_MODEL);
  appendRunnerArg(
    runnerArgs,
    "--provider",
    options.provider ?? env.GENTS_DESKTOP_LIVE_BACKEND_PROVIDER,
  );
  appendRunnerArg(
    runnerArgs,
    "--api-key",
    options.apiKey ?? env.GENTS_DESKTOP_LIVE_BACKEND_API_KEY,
  );
  appendRunnerArg(
    runnerArgs,
    "--api-key-env-var",
    options.apiKeyEnvVar ?? env.GENTS_DESKTOP_LIVE_BACKEND_API_KEY_ENV_VAR,
  );
  appendRunnerArg(
    runnerArgs,
    "--agent-target-inference-url",
    options.agentTargetInferenceUrl ?? env.GENTS_DESKTOP_LIVE_TARGET_BACKEND_ENDPOINT,
  );
  appendRunnerArg(
    runnerArgs,
    "--agent-target-model-name",
    options.agentTargetModelName ?? env.GENTS_DESKTOP_LIVE_TARGET_BACKEND_MODEL,
  );
  appendRunnerArg(
    runnerArgs,
    "--agent-target-provider",
    options.agentTargetProvider ?? env.GENTS_DESKTOP_LIVE_TARGET_BACKEND_PROVIDER,
  );
  appendRunnerArg(
    runnerArgs,
    "--agent-target-api-key",
    options.agentTargetApiKey ?? env.GENTS_DESKTOP_LIVE_TARGET_BACKEND_API_KEY,
  );
  appendRunnerArg(
    runnerArgs,
    "--agent-target-api-key-env-var",
    options.agentTargetApiKeyEnvVar ??
      env.GENTS_DESKTOP_LIVE_TARGET_BACKEND_API_KEY_ENV_VAR,
  );
  return { command, runnerArgs };
}

/** The harness's switch variables, read with the falsey rule the runner's
 *  flags once used: unset, empty, 0, false, no, off, n and f are off. */
function enabled(value: string | undefined) {
  if (value === undefined) return false;
  return !["", "0", "false", "no", "off", "n", "f"].includes(
    value.trim().toLowerCase(),
  );
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
    readonly nodeDid: string,
    readonly toolRoot: string,
    readonly dataRoot: string | undefined,
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
      message.nodeDid,
      message.toolRoot,
      message.dataRoot,
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

  diagnostics() {
    return { stdout: this.logs.stdoutTail(), stderr: this.logs.stderrTail() };
  }

  async dispose() {
    await disposeRunnerProcess(this.process);
  }

  async getJson<T>(path: string) {
    return this.http.getJson<T>(path);
  }

  async postJson<T = unknown>(path: string, body: unknown, timeoutMs?: number) {
    return this.http.postJson<T>(path, body, timeoutMs);
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

  async fetchRetainedProviderReasoning(sessionId: string, requestId: string) {
    return await this.postJson<{ requestDocId: string; reasoningBySource: string[] }>(
      "/desktop/request/retained-reasoning",
      { sessionId, requestId },
    );
  }

  private async fetchVersion() {
    const response = await this.getJson<VersionResponse>("/desktop/version");
    return response.version;
  }
}
