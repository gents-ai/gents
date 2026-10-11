import { invoke } from "@tauri-apps/api/core";
import { createElement } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import {
  bridgeCommand,
  type DesktopClientSnapshot,
  type DesktopSessionSnapshot,
  type PendingQueueEditRequest,
} from "@source-inc/gents-desktop-client";
import { findAssistantResponseMarker } from "./nativeSimulatorE2eDom";
import { Markdown } from "../ui/screens/Markdown";

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

/** WebKit may pause animation frames while an unattended test window is covered. */
export function emulateNativeReducedMotion(
  target: Pick<Window, "matchMedia"> = window,
): void {
  const matchMedia = target.matchMedia.bind(target);
  target.matchMedia = (query) => {
    const media = matchMedia(query);
    if (query === "(prefers-reduced-motion: reduce)")
      Object.defineProperty(media, "matches", { value: true, configurable: true });
    return media;
  };
}

/** Compare canonical Markdown with the text produced by the actual transcript renderer. */
export function renderedAssistantResponse(response: string): string {
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    flushSync(() => root.render(createElement(Markdown, { children: response })));
    return container.textContent ?? "";
  } finally {
    flushSync(() => root.unmount());
  }
}

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
  emulateNativeReducedMotion();
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
    const persistedResponse = renderedAssistantResponse(prior.response);
    if (!persistedResponse.trim())
      throw new Error("Persisted Engineer response has no rendered text");
    await until(
      () => findAssistantResponseMarker(document, persistedResponse),
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
  const previousDetail = prior
    ? await invoke<DesktopSessionSnapshot | null>(
        bridgeCommand("desktop_session_snapshot"),
        {
          sessionId: prior.sessionId,
          nodeDid: deployment.nodeDid,
          requestId: prior.requestId,
        },
      )
    : null;
  if (prior && !previousDetail)
    throw new Error("Restored session has no canonical timeline before follow-up");
  const previousItems = new Set(
    previousDetail?.timelineItems.map((item) => item.itemKey) ?? [],
  );
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
  const gateUrl = `${config.endpoint.replace(/\/$/, "")}/__e2e_gate`;
  await until(async () => {
    const response = await fetch(gateUrl);
    if (!response.ok) throw new Error("Native provider gate is unavailable");
    const state = await response.json();
    return state.held === true && state.released === false ? true : null;
  }, "actual main provider response held before queue editing");
  const heldSession = await until(async () => {
    const current = await snapshot();
    return (
      current.client?.deployments
        .find((item) => item.nodeDid === deployment.nodeDid)
        ?.sessions.find(
          (item) =>
            item.latestRequestId &&
            !previous.has(item.latestRequestId) &&
            (!prior || item.sessionId === prior.sessionId),
        ) ?? null
    );
  }, "held request session");
  const heldRequestId = heldSession.latestRequestId!;
  const queueDetail = () =>
    invoke<DesktopSessionSnapshot | null>(bridgeCommand("desktop_session_snapshot"), {
      sessionId: heldSession.sessionId,
      nodeDid: deployment.nodeDid,
      requestId: heldRequestId,
    });
  const sendQueued = async (text: string) => {
    const input = await until(
      () =>
        document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message"]'),
      "active composer",
    );
    setValue(input, text);
    (
      await until(
        () =>
          document.querySelector<HTMLButtonElement>(
            'button[aria-label="Send"]:not(:disabled)',
          ),
        "send while active",
      )
    ).click();
    await until(
      async () =>
        (await queueDetail())?.pendingQueue?.some((entry) => entry.content === text)
          ? true
          : null,
      "durable pending message",
    );
  };
  const originalText = `QUEUE_${config.phase}_ORIGINAL`;
  const editedText = `QUEUE_${config.phase}_EDITED`;
  const secondText = `QUEUE_${config.phase}_SECOND`;
  const removedText = `QUEUE_${config.phase}_REMOVED`;
  for (const text of [originalText, secondText, removedText]) await sendQueued(text);
  const staleQueue = (await queueDetail())?.pendingQueue;
  const staleEntry = staleQueue?.find((entry) => entry.content === originalText);
  if (!staleQueue || !staleEntry)
    throw new Error("Pending edit snapshot is missing its original target");
  const pendingCard = (text: string) =>
    Array.from(
      document.querySelectorAll<HTMLElement>('[data-testid="queued-input"]'),
    ).find((item) => item.textContent?.includes(text)) ?? null;
  (
    await until(() => {
      const card = pendingCard(originalText);
      return card && exactControl(card, "button", "Edit");
    }, "edit pending message")
  ).click();
  const editInput = await until(
    () =>
      document.querySelector<HTMLTextAreaElement>(
        'textarea[aria-label="Edit pending message"]',
      ),
    "pending editor",
  );
  setValue(editInput, editedText);
  (
    await until(() => {
      const card = pendingCard(originalText);
      return card && exactControl(card, "button", "Save");
    }, "save pending edit")
  ).click();
  await until(async () => {
    const entries = (await queueDetail())?.pendingQueue ?? [];
    return entries.some((entry) => entry.content === editedText) &&
      !entries.some((entry) => entry.content === originalText)
      ? true
      : null;
  }, "runtime-acknowledged pending edit");
  const editedQueue = (await queueDetail())?.pendingQueue;
  if (
    !editedQueue ||
    editedQueue.some((entry) => entry.requestDocId === staleEntry.requestDocId)
  )
    throw new Error("Acknowledged edit did not replace its physical request");
  const queueIdentity = (entries: typeof editedQueue) =>
    entries.map(({ requestDocId, content }) => ({ requestDocId, content }));
  const beforeRejectedEdit = JSON.stringify(queueIdentity(editedQueue));
  const staleRequest: PendingQueueEditRequest = {
    nodeDid: deployment.nodeDid,
    sessionId: heldSession.sessionId,
    expectedRequestDocIds: staleQueue.map((entry) => entry.requestDocId),
    selectedRequestDocIds: [staleEntry.requestDocId],
    messages: [
      { requestDocId: staleEntry.requestDocId, content: `${originalText}_STALE` },
    ],
  };
  let staleError: unknown;
  try {
    await invoke(bridgeCommand("desktop_pending_queue_edit"), {
      request: staleRequest,
    });
  } catch (error) {
    staleError = error;
  }
  if (
    typeof staleError !== "object" ||
    staleError === null ||
    !("message" in staleError) ||
    typeof staleError.message !== "string" ||
    !staleError.message.includes("The message queue changed. Refresh it and try again.")
  )
    throw new Error(
      `Expected stale queue rejection, received ${JSON.stringify(staleError)}`,
    );
  const afterRejectedEdit = (await queueDetail())?.pendingQueue;
  if (
    !afterRejectedEdit ||
    JSON.stringify(queueIdentity(afterRejectedEdit)) !== beforeRejectedEdit
  )
    throw new Error("Rejected stale edit changed pending queue identity or content");
  const heldAfterRejection = await fetch(gateUrl);
  if (!heldAfterRejection.ok)
    throw new Error("Native provider gate is unavailable after rejection");
  const heldState = await heldAfterRejection.json();
  if (heldState.held !== true || heldState.released !== false)
    throw new Error("Provider response was released during stale edit rejection");
  const removedEntry = (await queueDetail())?.pendingQueue?.find(
    (entry) => entry.content === removedText,
  );
  if (!removedEntry) throw new Error("Removal target disappeared before UI action");
  (
    await until(() => {
      const card = pendingCard(removedText);
      return card && exactControl(card, "button", "Remove");
    }, "remove pending message")
  ).click();
  await until(async () => {
    const detail = await queueDetail();
    return detail?.pendingQueue &&
      !detail.pendingQueue.some((entry) => entry.content === removedText)
      ? true
      : null;
  }, "runtime-acknowledged removal");
  (
    await until(
      () =>
        pendingCard(secondText)?.querySelector<HTMLButtonElement>(
          'button[aria-label="Move pending message up"]:not(:disabled)',
        ) ?? null,
      "reorder pending messages",
    )
  ).click();
  const retainedEntries = await until(async () => {
    const entries = (await queueDetail())?.pendingQueue ?? [];
    return entries.length === 2 &&
      entries[0].content === secondText &&
      entries[1].content === editedText
      ? entries
      : null;
  }, "runtime-acknowledged queue order");
  await report({
    stage: "local-queue-edited",
    detail: JSON.stringify({ sessionId: heldSession.sessionId, heldRequestId }),
  });
  const released = await fetch(gateUrl, { method: "POST" });
  if (!released.ok) throw new Error("Could not release real provider response");
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
        requestId: heldRequestId,
      },
    );
    if (!detail) return null;
    if (
      detail.nodeDid !== deployment.nodeDid ||
      detail.sessionId !== summary.sessionId ||
      detail.latestRequestId !== heldRequestId
    )
      throw new Error("Engineer response did not match the submitted request scope");
    if (["failed", "cancelled", "interrupted"].includes(detail.turnState ?? ""))
      throw new Error(`Engineer request ended ${detail.turnState}`);
    return detail.turnState === "completed" ? detail : null;
  }, "terminal Engineer response");
  for (const entry of retainedEntries) {
    if (
      !session.foldedInputs.some(
        (folded) =>
          folded.requestId === entry.requestId &&
          folded.foldedIntoRequestId === heldRequestId,
      )
    )
      throw new Error("Retained queued input was not consumed by the held request");
  }
  if (
    session.foldedInputs.some((folded) => folded.requestId === removedEntry.requestId)
  )
    throw new Error("Removed queued input was consumed");
  if (
    !session.timelineItems.some(
      (item) =>
        item.kind === "assistantMessage" &&
        item.reconstruction.state === "ready" &&
        item.content?.includes(config.expectedResponse),
    )
  )
    throw new Error(
      "The original real provider response marker is missing from the durable timeline",
    );
  if (session.agentId !== engineer.agentId)
    throw new Error("Conversation used a different agent");
  const responses = session.timelineItems
    .filter(
      (item) =>
        item.kind === "assistantMessage" &&
        item.reconstruction.state === "ready" &&
        !previousItems.has(item.itemKey),
    )
    .map((item) => (item.kind === "assistantMessage" ? item.content : null))
    .filter((content): content is string => Boolean(content?.trim()));
  const response = responses[responses.length - 1];
  if (!response) throw new Error("Completed Engineer request has no durable response");
  const renderedResponse = renderedAssistantResponse(response);
  if (!renderedResponse.trim())
    throw new Error("Completed Engineer response has no rendered text");
  await until(
    () => findAssistantResponseMarker(document, renderedResponse),
    "durable Engineer response in native transcript",
  );
  const evidence: Evidence = {
    nodeDid: deployment.nodeDid,
    sessionId: session.sessionId,
    agentId: engineer.agentId,
    requestId: session.latestRequestId!,
    response,
  };
  localStorage.setItem(EVIDENCE_KEY, JSON.stringify(evidence));
  await report({
    stage: `local-${config.phase}-passed`,
    detail: JSON.stringify(evidence),
  });
}
