import { invoke } from "@tauri-apps/api/core";
import {
  bridgeCommand,
  type DesktopClientSnapshot,
  type DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { findAssistantResponseMarker } from "./nativeSimulatorE2eDom";

export type NativeLocalSetup = {
  endpoint: string;
  model: string;
  phase: string;
  toolRoot: string;
};

type Config = NativeLocalSetup & {
  agentLabel: string;
  prompt: string;
  expectedResponse: string;
};
type Evidence = {
  nodeDid: string;
  sessionId: string;
  agentId: string;
  requestId: string;
  response: string;
};
const EVIDENCE_KEY = "gents-native-local-e2e";

export function labelledInput(root: ParentNode, label: string) {
  const matched = Array.from(root.querySelectorAll("label")).find(
    (element) =>
      (element.querySelector("span")?.textContent ?? element.textContent)?.trim() ===
      label,
  );
  return matched?.control instanceof HTMLInputElement
    ? matched.control
    : (matched?.querySelector<HTMLInputElement>("input") ?? null);
}

export function exactControl(root: ParentNode, selector: string, text: string) {
  return (
    Array.from(root.querySelectorAll<HTMLElement>(selector)).find(
      (element) =>
        (element.textContent?.trim() === text ||
          Array.from(element.querySelectorAll("span")).some(
            (span) =>
              Array.from(span.childNodes)
                .filter((node) => node.nodeType === Node.TEXT_NODE)
                .map((node) => node.textContent)
                .join("")
                .trim() === text,
          )) &&
        !element.hasAttribute("disabled"),
    ) ?? null
  );
}

export function persistedSessionControl(root: ParentNode, sessionId: string) {
  const session = root.querySelector<HTMLElement>(
    `[data-testid="session-${CSS.escape(sessionId)}"]`,
  );
  if (session) return { kind: "session" as const, element: session };
  const list =
    exactControl(root, "a", "Sessions") ??
    root.querySelector<HTMLElement>('a[aria-label="Sessions"]');
  return list ? { kind: "list" as const, element: list } : null;
}

function setValue(element: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const prototype =
    element instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(prototype, "value")!.set!.call(element, value);
  element.dispatchEvent(new Event("input", { bubbles: true }));
  element.dispatchEvent(new Event("change", { bubbles: true }));
}

async function until<T>(
  sample: () => T | null | Promise<T | null>,
  description: string,
) {
  const deadline = performance.now() + 300_000;
  while (performance.now() < deadline) {
    const result = await sample();
    if (result) return result;
    const alert = document.querySelector(
      '[role="alert"], [data-testid="error-banner"]',
    );
    if (alert?.textContent?.trim()) throw new Error(alert.textContent.trim());
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`Timed out waiting for ${description}`);
}

const snapshot = () =>
  invoke<DesktopClientSnapshot>(bridgeCommand("desktop_client_snapshot"));

/** Runs only inside the native test build; every mutation goes through visible UI. */
export async function runNativeLocalE2e(
  config: Config,
  report: (status: { stage: string; detail?: string }) => Promise<void>,
) {
  if (!["setup", "reopen"].includes(config.phase))
    throw new Error("Unknown local E2E phase");
  if (!config.toolRoot) throw new Error("Local E2E requires an isolated tool root");
  await report({ stage: `local-${config.phase}-starting` });
  if (config.phase === "setup") {
    const name = await until(
      () => labelledInput(document, "Node name"),
      "native first-run setup",
    );
    if (!name.readOnly) setValue(name, config.agentLabel);
    const root = await until(
      () => labelledInput(document, "Tool root"),
      "tool-root control",
    );
    root.focus();
    setValue(root, config.toolRoot);
    root.blur();
    (
      await until(
        () =>
          document.querySelector<HTMLButtonElement>(
            '[data-testid="setup-next"]:not(:disabled)',
          ),
        "start local managed service",
      )
    ).click();
    await report({ stage: "local-service-starting" });
    (
      await until(
        () =>
          document.querySelector<HTMLButtonElement>(
            '[data-testid="setup-provider-local"]:not(:disabled)',
          ),
        "inference provider setup",
      )
    ).click();
    setValue(
      await until(() => labelledInput(document, "Endpoint"), "inference endpoint"),
      config.endpoint,
    );
    (
      await until(
        () => exactControl(document, "button", "Connect and find models"),
        "model discovery",
      )
    ).click();
    (
      await until(
        () => exactControl(document, '[role="option"]', config.model),
        "advertised GLM model",
      )
    ).click();
    setValue(
      await until(() => labelledInput(document, "Temperature"), "temperature control"),
      "1",
    );
    await new Promise((resolve) => setTimeout(resolve, 0));
    setValue(
      await until(() => labelledInput(document, "Top-p"), "top-p control"),
      "0.95",
    );
    (
      await until(
        () => exactControl(document, "button", "Advanced settings"),
        "reasoning settings",
      )
    ).click();
    (
      await until(
        () => document.querySelector<HTMLButtonElement>("#inference-reasoning"),
        "advertised reasoning effort control",
      )
    ).click();
    (
      await until(
        () => exactControl(document, '[role="option"]', "high"),
        "advertised high reasoning effort",
      )
    ).click();
    (
      await until(
        () =>
          document.querySelector<HTMLButtonElement>(
            '[data-testid="setup-save-inference"]:not(:disabled)',
          ),
        "save inference",
      )
    ).click();
    await until(
      () =>
        document.querySelector(
          '[data-testid="session-screen"] button[aria-label="Agent"]',
        ),
      "session composer after inference was saved",
    );
    await report({ stage: "local-inference-saved" });
  }

  const prior: Evidence | null =
    config.phase === "reopen"
      ? JSON.parse(localStorage.getItem(EVIDENCE_KEY) ?? "null")
      : null;
  if (config.phase === "reopen" && !prior)
    throw new Error("Restart has no prior native session evidence");
  const deployment = await until(async () => {
    const current = await snapshot();
    return (
      current.client?.deployments.find((item) =>
        prior
          ? item.nodeDid === prior.nodeDid
          : item.node.displayName === config.agentLabel,
      ) ?? null
    );
  }, "persisted node deployment");
  const engineer = deployment.agents.find(
    (item) => item.displayName === "The Engineer" && item.enabled,
  );
  if (!engineer) throw new Error("The Engineer agent is absent or disabled");
  const profile = deployment.inferenceProfiles.find(
    (item) => item.profile_id === engineer.inferenceProfileId,
  );
  const backend = deployment.inferenceBackends.find(
    (item) => item.backendId === profile?.backend_id,
  );
  const sampling = deployment.inferenceSampling.find(
    (item) => item.sampling_id === profile?.sampling_id,
  );
  if (
    profile?.model_name !== config.model ||
    backend?.endpoint?.replace(/\/+$/, "") !== config.endpoint.replace(/\/+$/, "")
  ) {
    throw new Error(
      "The Engineer did not retain the UI-selected inference configuration",
    );
  }
  if (
    profile.reasoning_effort !== "high" ||
    sampling?.temperature !== 1 ||
    sampling?.top_p !== 0.95
  ) {
    throw new Error(
      "The Engineer did not retain high reasoning, temperature 1 and top-p 0.95",
    );
  }

  if (prior) {
    await report({ stage: "local-session-restoring" });
    const navigation = await until(
      () => persistedSessionControl(document, prior.sessionId),
      "persisted session or session-list navigation",
    );
    if (navigation.kind === "list") navigation.element.click();
    (
      await until(
        () =>
          document.querySelector<HTMLElement>(
            `[data-testid="session-${CSS.escape(prior.sessionId)}"]`,
          ),
        "persisted session in session list",
      )
    ).click();
    await until(
      () => findAssistantResponseMarker(document, prior.response),
      "persisted assistant response",
    );
    await report({ stage: "local-session-restored" });
  } else {
    (
      await until(
        () => document.querySelector<HTMLButtonElement>('button[aria-label="Agent"]'),
        "new-session agent picker",
      )
    ).click();
    (
      await until(
        () => exactControl(document, '[role="option"]', "The Engineer"),
        "The Engineer selection",
      )
    ).click();
  }
  const composer = await until(
    () => document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message"]'),
    "Engineer composer",
  );
  setValue(composer, config.prompt);
  const previous = new Set(
    deployment.sessions.flatMap((item) =>
      item.latestRequestId ? [item.latestRequestId] : [],
    ),
  );
  (
    await until(
      () =>
        document.querySelector<HTMLButtonElement>(
          'button[aria-label="Send"]:not(:disabled)',
        ),
      "enabled Engineer send",
    )
  ).click();
  await report({ stage: "local-chat-sent" });
  const session = await until(async () => {
    const current = await snapshot();
    const summary = current.client?.deployments
      .find((item) => item.nodeDid === deployment.nodeDid)
      ?.sessions.find(
        (item) =>
          item.latestRequestId &&
          !previous.has(item.latestRequestId) &&
          (!prior || item.sessionId === prior.sessionId),
      );
    if (!summary?.latestRequestId) return null;
    const detail = await invoke<DesktopSessionSnapshot | null>(
      bridgeCommand("desktop_session_snapshot"),
      {
        sessionId: summary.sessionId,
        nodeDid: deployment.nodeDid,
        requestId: summary.latestRequestId,
      },
    );
    if (!detail) return null;
    if (["failed", "cancelled", "interrupted"].includes(detail.turnState ?? ""))
      throw new Error(`Engineer request ended ${detail.turnState}`);
    return detail.turnState === "completed" ? detail : null;
  }, "terminal Engineer response");
  if (session.agentId !== engineer.agentId)
    throw new Error("Conversation used a different agent");
  await until(
    () => findAssistantResponseMarker(document, config.expectedResponse),
    "Engineer response in native transcript",
  );
  const evidence: Evidence = {
    nodeDid: deployment.nodeDid,
    sessionId: session.sessionId,
    agentId: engineer.agentId,
    requestId: session.latestRequestId!,
    response: config.expectedResponse,
  };
  localStorage.setItem(EVIDENCE_KEY, JSON.stringify(evidence));
  await report({
    stage: `local-${config.phase}-passed`,
    detail: JSON.stringify(evidence),
  });
}
