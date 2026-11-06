import { invoke } from "@tauri-apps/api/core";
import {
  bridgeCommand,
  type DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { runNativeLocalE2e, type NativeLocalSetup } from "./nativeLocalE2e";

import {
  sessionRowCount,
  completedReconnectSession,
  findAgentChatButton,
  findAgentDeploymentControl,
  findAssistantResponseMarker,
  findNewChatButton,
  isSessionTurnSettled,
} from "./nativeSimulatorE2eDom";
import { errorText } from "./errorText";

export {
  sessionRowCount,
  findAgentChatButton,
  findAgentDeploymentControl,
  findAssistantResponseMarker,
  findNewChatButton,
  isSessionTurnSettled,
} from "./nativeSimulatorE2eDom";

type NativeE2eConfig = {
  agentLabel: string;
  serverAddress: string;
  prompt: string;
  expectedResponse: string;
  expectEmptySessionSlice: boolean;
  correlationId: string;
  reconnectRealInference: boolean;
  measurePerformance: boolean;
  localSetup?: NativeLocalSetup | null;
};

type NativeE2eStatus = {
  stage: string;
  detail?: string;
  correlationId?: string;
  monotonicMs?: number;
  metrics?: {
    ui: {
      sessionRows: number;
      transcriptCards: number;
      transcriptTurnBlocks: number;
      bodyBytes: number;
    };
    observer: unknown;
    observerTimedOut: boolean;
  };
};

type TauriBridgeWindow = Window & {
  __TAURI_INTERNALS__?: {
    invoke?: unknown;
  };
};

let activeRun: Promise<void> | null = null;
let activeConfig: NativeE2eConfig | null = null;

export function startNativeSimulatorE2e(
  enabled = import.meta.env.VITE_GENTS_NATIVE_E2E === "1",
): Promise<void> {
  if (!enabled) {
    return Promise.resolve();
  }
  if (activeRun) {
    return activeRun;
  }
  if (typeof (window as TauriBridgeWindow).__TAURI_INTERNALS__?.invoke !== "function") {
    return Promise.resolve();
  }

  activeRun = runNativeSimulatorE2e().catch(async (error) => {
    const detail = errorText(error);
    const status = { stage: "failed", detail };
    renderStatus(status);
    await invoke(bridgeCommand("desktop_native_e2e_status"), { status }).catch(
      () => {},
    );
  });
  return activeRun;
}

async function runNativeSimulatorE2e() {
  const config = await invoke<NativeE2eConfig | null>(
    bridgeCommand("desktop_native_e2e_config"),
  );
  if (!config) {
    return;
  }
  activeConfig = config;

  if (config.localSetup) {
    await runNativeLocalE2e(
      {
        ...config.localSetup,
        agentLabel: config.agentLabel,
        prompt: config.prompt,
        expectedResponse: config.expectedResponse,
      },
      reportStatus,
    );
    return;
  }

  try {
    await reportStatus({ stage: "starting" });
    await waitFor(
      () =>
        /Sessions|Let’s get set up|Connect to your node|Nodes/.test(
          document.body.textContent ?? "",
        )
          ? document.body
          : null,
      30_000,
      "kit shell",
    );
    await reportStatus({ stage: "shell-interactive" });
    if (document.querySelector('[data-testid="fleet-connect-local"]')) {
      throw new Error("Mobile shell exposed unsupported local runtime setup");
    }

    const deploymentControl = await waitForOptional(
      () => findAgentDeploymentControl(config.agentLabel),
      15_000,
    );
    if (!deploymentControl) {
      await reportStatus({ stage: "enrollment" });
      await enrollNode(config);
      await waitFor(
        () =>
          findAgentDeploymentControl(config.agentLabel) ??
          (config.correlationId.startsWith("ios-reconnect-")
            ? findNewSessionScreen(config.agentLabel)
            : null),
        300_000,
        `${config.agentLabel} deployment`,
      );
    }
    if (config.correlationId.startsWith("ios-reconnect-")) {
      await runReconnectScenario(config);
      await reportStatus({ stage: "passed" });
      return;
    }
    await reportStatus({ stage: "waiting-ready" });
    const currentChatButton = await waitFor(
      () => findAgentChatButton(config.agentLabel),
      300_000,
      `${config.agentLabel} enabled chat control after signed enrollment readiness`,
    );
    currentChatButton.click();
    await waitFor(
      () => document.querySelector<HTMLButtonElement>('[data-testid="session-new"]'),
      30_000,
      "visible session index",
    );
    await reportStatus({ stage: "session-index-visible" });

    if (config.expectEmptySessionSlice) {
      const sessionCount = sessionRowCount(document);
      if (sessionCount > 0) {
        throw new Error(
          `Requester-scoped enrollment leaked ${sessionCount} pre-existing session(s)`,
        );
      }
    }

    const environmentReadinessDeadline = window.performance.now() + 300_000;
    const chooseEnvironmentButton = await waitFor(
      () => {
        const button = document.querySelector<HTMLButtonElement>(
          '[data-testid="session-new"]',
        );
        return button && !button.disabled ? button : null;
      },
      remainingMs(environmentReadinessDeadline),
      `${config.agentLabel} session creation readiness`,
    );
    chooseEnvironmentButton.click();

    const newChatButton = await waitFor(
      () => findNewChatButton(config.agentLabel),
      remainingMs(environmentReadinessDeadline),
      `${config.agentLabel} unique enabled agent readiness`,
    );
    newChatButton.click();

    const composer = await waitFor(
      () =>
        document.querySelector<HTMLTextAreaElement>(
          '[data-testid="composer"] textarea, textarea[aria-label="Message"]',
        ),
      30_000,
      "chat composer",
    );
    await reportStatus({ stage: "ready" });
    setControlledValue(composer, config.prompt);

    const sendButton = await waitFor(
      () => {
        const button = document.querySelector<HTMLButtonElement>(
          '[data-testid="composer"] button[aria-label="Send"], button[aria-label="Send"]',
        );
        return button && !button.disabled ? button : null;
      },
      30_000,
      "send button after entering the prompt",
    );

    await reportStatus({ stage: "chat-open" });
    sendButton.click();
    await reportStatus({ stage: "sent" });

    await waitFor(
      () => findAssistantResponseMarker(document, config.expectedResponse),
      180_000,
      `${config.agentLabel} response marker ${config.expectedResponse}`,
    );

    await reportStatus({ stage: "waiting-terminal" });
    await waitFor(
      () =>
        isSessionTurnSettled(document, config.expectedResponse) ? document.body : null,
      180_000,
      `${config.agentLabel} terminal turn state`,
    );

    await reportStatus({ stage: "passed" });
  } catch (error) {
    const detail = errorText(error);
    await reportStatus({ stage: "failed", detail }).catch(() => {});
  }
}

async function runReconnectScenario(config: NativeE2eConfig) {
  const snapshot = () =>
    invoke<DesktopClientSnapshot>(bridgeCommand("desktop_client_snapshot"));
  const metrics = () =>
    invoke<{ events_received: number } | null>(
      bridgeCommand("desktop_observer_metrics"),
    );
  const initial = await waitForAsync(async () => {
    const deployments = (await snapshot()).client?.deployments ?? [];
    const matches = deployments.filter(
      (deployment) => deployment.label === config.agentLabel,
    );
    return matches.length === 1 && matches[0].runtimeConnection === "connected"
      ? matches[0]
      : null;
  }, "connected isolated runtime");
  const nodeDid = initial.nodeDid;
  const agent = initial.agentConfigs.find(
    (row) => row.agent_did === initial.node.defaultAgentDid,
  );
  if (!agent) throw new Error("Isolated reconnect agent is missing");
  if (!findNewSessionScreen(config.agentLabel)) {
    const chat = await waitFor(
      () => findAgentChatButton(config.agentLabel),
      180_000,
      "isolated runtime chat control",
    );
    chat.click();
    const newSession = await waitFor(
      () =>
        document.querySelector<HTMLButtonElement>(
          '[data-testid="session-new"]:not(:disabled)',
        ),
      180_000,
      "isolated runtime new-session control",
    );
    newSession.click();
    const newChat = await waitFor(
      () => findNewChatButton(config.agentLabel),
      180_000,
      "isolated runtime new-chat control",
    );
    newChat.click();
  }
  await waitFor(
    () => findNewSessionScreen(config.agentLabel),
    30_000,
    "new-session screen",
  );
  if (config.reconnectRealInference) {
    await runReconnectInferenceTurn(config, nodeDid, agent.agent_did, "before");
    await returnToNewSession(config.agentLabel);
  }
  const offlineName = `${config.correlationId}-offline`;
  const liveName = `${config.correlationId}-live`;
  const deployment = async () =>
    (await snapshot()).client?.deployments.find((row) => row.nodeDid === nodeDid);

  await reportStatus({ stage: "reconnect-arm" });
  await waitForAsync(
    async () =>
      (await deployment())?.runtimeConnection === "disconnected" ? true : null,
    "runtime disconnection",
  );
  await reportStatus({ stage: "reconnect-offline" });
  await waitForAsync(async () => {
    const row = await deployment();
    return row?.runtimeConnection === "connected" &&
      row.agentConfigs.some(
        (item) =>
          item.agent_did === agent.agent_did && item.display_name === offlineName,
      ) &&
      document
        .querySelector('[data-testid="session-screen"]')
        ?.textContent?.includes(offlineName)
      ? true
      : null;
  }, "offline change in the reconnected snapshot and UI");
  const baseline = await metrics();
  if (!baseline) throw new Error("Observer metrics are unavailable");
  await reportStatus({ stage: "reconnect-caught-up" });
  await waitForAsync(async () => {
    const row = await deployment();
    const observed = await metrics();
    return row?.runtimeConnection === "connected" &&
      row.agentConfigs.some(
        (item) => item.agent_did === agent.agent_did && item.display_name === liveName,
      ) &&
      document
        .querySelector('[data-testid="session-screen"]')
        ?.textContent?.includes(liveName) &&
      observed &&
      observed.events_received > baseline.events_received
      ? true
      : null;
  }, "post-reconnect live change in the snapshot and UI");
  if (config.reconnectRealInference) {
    await runReconnectInferenceTurn(config, nodeDid, agent.agent_did, "after");
  }
}

async function runReconnectInferenceTurn(
  config: NativeE2eConfig,
  nodeDid: string,
  agentDid: string,
  phase: "before" | "after",
) {
  const initial = await invoke<DesktopClientSnapshot>(
    bridgeCommand("desktop_client_snapshot"),
  );
  const clientDid = initial.client?.clientDid;
  const deployment = initial.client?.deployments.find((row) => row.nodeDid === nodeDid);
  if (!clientDid || !deployment)
    throw new Error("Held client or runtime is unavailable");
  if (clientDid === nodeDid || clientDid === agentDid)
    throw new Error("Reconnect inference requires a distinct held client identity");
  const existingSessions = new Set(
    deployment.sessions.map((session) => session.sessionId),
  );
  const suffix = config.correlationId.replace(/[^A-Za-z0-9]/g, "").toUpperCase();
  const marker = `GENTS_RECONNECT_${phase.toUpperCase()}_${suffix}`;
  const prompt = `Reply with exactly this text and nothing else: ${marker}`;
  const editor = await waitFor(
    () => {
      const field = document.querySelector<HTMLElement>(
        '[data-testid="composer"] [contenteditable="true"]',
      );
      return field && !field.closest("[inert]") ? field : null;
    },
    30_000,
    `${phase} reconnect chat composer`,
  );
  editor.focus();
  const selection = window.getSelection();
  if (!selection) throw new Error("Cannot select the chat composer");
  const range = document.createRange();
  range.selectNodeContents(editor);
  selection.removeAllRanges();
  selection.addRange(range);
  if (!document.execCommand("insertText", false, prompt)) {
    throw new Error("Could not enter the reconnect inference prompt");
  }
  const send = await waitFor(
    () => {
      const button = document.querySelector<HTMLButtonElement>(
        '[data-testid="composer"] button[aria-label="Send"]',
      );
      return button && !button.disabled ? button : null;
    },
    30_000,
    `${phase} reconnect send button`,
  );
  send.click();
  const completed = await waitForAsync(async () => {
    const snapshot = await invoke<DesktopClientSnapshot>(
      bridgeCommand("desktop_client_snapshot"),
    );
    const current = snapshot.client?.deployments.find((row) => row.nodeDid === nodeDid);
    const session = completedReconnectSession(
      current?.sessions ?? [],
      existingSessions,
      agentDid,
      clientDid,
    );
    if (!session || !findAssistantResponseMarker(document, marker)) return null;
    return {
      nodeDid,
      clientDid,
      agentDid,
      sessionId: session.sessionId,
      requestId: session.latestRequestId,
    };
  }, `${phase} reconnect inference completion`);
  await reportStatus({
    stage: `reconnect-${phase}-turn`,
    detail: JSON.stringify(completed),
  });
}

async function returnToNewSession(label: string) {
  const menu = await waitFor(
    () => document.querySelector<HTMLButtonElement>('button[aria-label="Menu"]'),
    30_000,
    "mobile navigation menu",
  );
  menu.click();
  const newSession = await waitFor(
    () =>
      Array.from(document.querySelectorAll<HTMLElement>('[role="dialog"] a span'))
        .find((label) => label.textContent?.trim() === "New session")
        ?.closest<HTMLAnchorElement>("a") ?? null,
    30_000,
    "new-session navigation",
  );
  newSession.click();
  await waitFor(
    () => findNewSessionScreen(label),
    30_000,
    "new-session screen after inference",
  );
}

function findNewSessionScreen(label: string): HTMLElement | null {
  const screen = document.querySelector<HTMLElement>('[data-testid="session-screen"]');
  return screen?.textContent?.includes(label) ? screen : null;
}

async function waitForAsync<T>(
  sample: () => Promise<T | null>,
  description: string,
): Promise<T> {
  const deadline = window.performance.now() + 180_000;
  while (window.performance.now() < deadline) {
    const value = await sample();
    if (value) return value;
    await delay(250);
  }
  throw new Error(`Timed out waiting for ${description}`);
}

async function enrollNode(config: NativeE2eConfig) {
  const onboardingInput = await waitFor(
    () =>
      document.querySelector<HTMLInputElement>(
        'input[placeholder="https://gents.example.net:8787"]',
      ),
    30_000,
    "server address input",
  );
  setControlledValue(onboardingInput, config.serverAddress);

  const submit = await waitFor(
    () => findEnabledButton("Request access"),
    10_000,
    "enabled enrollment request button",
  );
  submit.click();

  const requestId = await waitFor(
    () => {
      const label = Array.from(document.querySelectorAll("dt")).find(
        (item) => item.textContent?.trim() === "Request",
      );
      return label?.nextElementSibling?.textContent?.trim() || null;
    },
    30_000,
    "enrollment request ID",
  );
  await reportStatus({ stage: "enrollment-pending", detail: requestId });
  const proceed = await waitFor(
    () => findEnabledButton("Continue"),
    300_000,
    "approved enrollment continuation",
  );
  proceed.click();
}

function findEnabledButton(label: string): HTMLButtonElement | null {
  return (
    Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) => button.textContent?.trim() === label && !button.disabled,
    ) ?? null
  );
}

function setControlledValue(
  element: HTMLInputElement | HTMLTextAreaElement,
  value: string,
) {
  const prototype =
    element instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set;
  if (!setter) {
    throw new Error(`Could not set ${element.tagName.toLowerCase()} value`);
  }
  setter.call(element, value);
  element.dispatchEvent(new Event("input", { bubbles: true }));
  element.dispatchEvent(new Event("change", { bubbles: true }));
}

async function waitFor<T>(
  sample: () => T | null | undefined,
  timeoutMs: number,
  description: string,
): Promise<T> {
  const deadline = window.performance.now() + timeoutMs;
  while (window.performance.now() < deadline) {
    const value = sample();
    if (value) {
      return value;
    }
    const enrollmentError = document.querySelector<HTMLElement>('[role="alert"]');
    if (enrollmentError?.textContent?.trim()) {
      throw new Error(enrollmentError.textContent.trim());
    }
    const globalError = document.querySelector<HTMLElement>(
      '[data-testid="error-banner"] .error-banner-message',
    );
    if (globalError?.textContent?.trim()) {
      throw new Error(globalError.textContent.trim());
    }
    await delay(250);
  }
  throw new Error(`Timed out waiting for ${description}`);
}

async function waitForOptional<T>(
  sample: () => T | null | undefined,
  timeoutMs: number,
): Promise<T | null> {
  const deadline = window.performance.now() + timeoutMs;
  while (window.performance.now() < deadline) {
    const value = sample();
    if (value) {
      return value;
    }
    await delay(250);
  }
  return null;
}

async function reportStatus(status: NativeE2eStatus) {
  const observerResult = activeConfig?.measurePerformance
    ? await Promise.race([
        invoke(bridgeCommand("desktop_observer_metrics"))
          .then((observer) => ({ observer, timedOut: false }))
          .catch(() => ({ observer: null, timedOut: false })),
        delay(1_000).then(() => ({ observer: null, timedOut: true })),
      ])
    : null;
  const measured = activeConfig?.measurePerformance
    ? {
        ...status,
        correlationId: activeConfig.correlationId,
        monotonicMs: window.performance.now(),
        metrics: {
          ui: {
            sessionRows: sessionRowCount(document),
            transcriptCards: document.querySelectorAll(
              '[data-testid="transcript-panel"] .message-card',
            ).length,
            transcriptTurnBlocks: document.querySelectorAll(
              '[data-testid="transcript-panel"] .turn-block',
            ).length,
            bodyBytes: new TextEncoder().encode(document.body.innerHTML).byteLength,
          },
          observer: observerResult?.observer ?? null,
          observerTimedOut: observerResult?.timedOut ?? false,
        },
      }
    : status;
  renderStatus(measured);
  await invoke(bridgeCommand("desktop_native_e2e_status"), { status: measured });
}

function renderStatus(status: NativeE2eStatus) {
  let banner = document.querySelector<HTMLElement>("#gents-native-e2e-status");
  if (!banner) {
    banner = document.createElement("aside");
    banner.id = "gents-native-e2e-status";
    banner.setAttribute("role", "status");
    Object.assign(banner.style, {
      background: "#00150d",
      border: "1px solid #00d477",
      bottom: "12px",
      color: "#dcfff0",
      font: "600 12px/1.4 ui-monospace, monospace",
      left: "12px",
      maxWidth: "calc(100vw - 24px)",
      padding: "8px 10px",
      position: "fixed",
      zIndex: "2147483647",
    });
    document.body.append(banner);
  }
  const detail = status.detail ? ` · ${status.detail}` : "";
  banner.textContent = `Native E2E: ${status.stage}${detail}`;
}

function delay(milliseconds: number) {
  return new Promise((resolve) => window.setTimeout(resolve, milliseconds));
}

function remainingMs(deadline: number) {
  return Math.max(0, deadline - window.performance.now());
}
