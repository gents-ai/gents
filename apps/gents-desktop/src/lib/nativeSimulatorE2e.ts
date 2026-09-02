import { invoke } from "@tauri-apps/api/core";
import { bridgeCommand } from "@source-inc/gents-desktop-client";

import {
  conversationRowCount,
  findAgentChatButton,
  findAgentDeploymentControl,
  findAssistantResponseMarker,
  findNewChatButton,
  isConversationTurnSettled,
} from "./nativeSimulatorE2eDom";

export {
  conversationRowCount,
  findAgentChatButton,
  findAgentDeploymentControl,
  findAssistantResponseMarker,
  findNewChatButton,
  isConversationTurnSettled,
} from "./nativeSimulatorE2eDom";

type NativeE2eConfig = {
  agentLabel: string;
  serverAddress: string;
  prompt: string;
  expectedResponse: string;
  expectEmptyConversationSlice: boolean;
  correlationId: string;
  measurePerformance: boolean;
};

type NativeE2eStatus = {
  stage: string;
  detail?: string;
  correlationId?: string;
  monotonicMs?: number;
  metrics?: {
    ui: {
      conversationRows: number;
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
    const detail = error instanceof Error ? error.message : String(error);
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

  try {
    await reportStatus({ stage: "starting" });
    await waitForText("Fleet Dashboard", 30_000);
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
      await enrollAgent(config);
      await waitFor(
        () => findAgentDeploymentControl(config.agentLabel),
        300_000,
        `${config.agentLabel} deployment`,
      );
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

    if (config.expectEmptyConversationSlice) {
      const conversationCount = conversationRowCount(document);
      if (conversationCount > 0) {
        throw new Error(
          `Requester-scoped enrollment leaked ${conversationCount} pre-existing conversation(s)`,
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
      `${config.agentLabel} unique enabled behavior readiness`,
    );
    newChatButton.click();

    const composer = await waitFor(
      () =>
        document.querySelector<HTMLTextAreaElement>('[data-testid="composer-input"]'),
      30_000,
      "chat composer",
    );
    await reportStatus({ stage: "ready" });
    setControlledValue(composer, config.prompt);

    const sendButton = await waitFor(
      () => {
        const button = document.querySelector<HTMLButtonElement>(
          '[data-testid="composer-send"]',
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
        isConversationTurnSettled(document, config.expectedResponse)
          ? document.body
          : null,
      180_000,
      `${config.agentLabel} terminal turn state`,
    );

    await reportStatus({ stage: "waiting-hydration" });
    await waitFor(
      () => {
        const status = document.querySelector<HTMLElement>(
          '[data-testid="conversation-loading-status"]',
        );
        if (
          status?.dataset.loadingLayer === "sessionSync" &&
          status.dataset.loadingPhase === "failed"
        ) {
          throw new Error(status.textContent?.trim() || "Session hydration failed");
        }
        return status?.dataset.loadingLayer === "sessionSync" ? null : document.body;
      },
      180_000,
      `${config.agentLabel} completed session hydration`,
    );

    await reportStatus({ stage: "passed" });
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    await reportStatus({ stage: "failed", detail }).catch(() => {});
  }
}

async function enrollAgent(config: NativeE2eConfig) {
  const disclosure = await waitFor(
    () =>
      document.querySelector<HTMLElement>(
        '[data-testid="fleet-remote-disclosure"] summary',
      ),
    30_000,
    "remote-agent disclosure",
  );
  disclosure.click();

  const serverAddressInput = await waitFor(
    () =>
      document.querySelector<HTMLInputElement>(
        '[data-testid="fleet-add-server-address"]',
      ),
    10_000,
    "server address input",
  );
  setControlledValue(serverAddressInput, config.serverAddress);

  const submit = await waitFor(
    () => {
      const button = document.querySelector<HTMLButtonElement>(
        '[data-testid="fleet-fetch-status"]',
      );
      return button && !button.disabled ? button : null;
    },
    10_000,
    "enabled enrollment request button",
  );
  submit.click();

  const requestId = await waitFor(
    () =>
      document
        .querySelector<HTMLElement>('[data-testid="fleet-import-status"]')
        ?.textContent?.match(/Enrollment request (\S+) sent/)?.[1] ?? null,
    30_000,
    "enrollment request ID",
  );
  await reportStatus({ stage: "enrollment-pending", detail: requestId });
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

async function waitForText(expected: string, timeoutMs: number) {
  await waitFor(
    () => (document.body.textContent?.includes(expected) ? document.body : null),
    timeoutMs,
    `visible text ${expected}`,
  );
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
    const enrollmentError = document.querySelector<HTMLElement>(
      '[data-testid="fleet-import-status"].fleet-inline-error',
    );
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
            conversationRows: conversationRowCount(document),
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
