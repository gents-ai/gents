import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChatTranscriptPanel } from "@source-inc/gents-desktop-chat";
import { MessageList } from "@source-inc/gents-desktop-chat";
import { createChatActions } from "../src/hooks/chatActions";
import {
  projectDeploymentOperationalState,
  type NodeReadinessDecision,
  type DesktopApiAdapter,
} from "@source-inc/gents-desktop-client";
import { projectChatShell } from "@source-inc/gents-desktop-chat";
import { copyText } from "@source-inc/gents-desktop-ui";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { deployment } from "./config-panel-wiring/fixtures";
import type { ShellProjection } from "../src/hooks/shellProjection";
import { shellStores } from "./shell-fixture";
import { assistantMessage, sessionSnapshot, userMessage } from "./timeline-fixture";

/** chat actions on session s1 of `deployment`, admitted by the projections given */
function chatActions(
  api: DesktopApiAdapter,
  projection: {
    agentReadiness: NodeReadinessDecision;
    shellProjection: ReturnType<typeof projectChatShell>;
    retryShellProjection: ReturnType<typeof projectChatShell>;
  },
  reportFailure = vi.fn(),
) {
  const stores = shellStores({
    deployments: [deployment],
    selection: { nodeDid: deployment.nodeDid, sessionId: "s1" },
  });
  const actions = createChatActions({
    api,
    stores,
    project: () => projection as unknown as ShellProjection,
    refreshSession: vi.fn(),
    refreshSnapshot: vi.fn(),
    reportFailure,
  });
  return { actions, stores };
}

const readyNodeReadiness = {
  kind: "ready",
  agentId: "default",
  agentLabel: "Default",
} as const;
const unavailableNodeReadiness = {
  kind: "unavailable",
  agentId: "ops",
  agentLabel: "Ops",
  reason: "backend_temporarily_unavailable",
} as const;

function operationalStateFor(
  decision: NodeReadinessDecision = readyNodeReadiness,
  routeReady = deployment.chatSafe,
) {
  const agentId = decision.agentId ?? "default";
  return projectDeploymentOperationalState({
    ...deployment,
    chatSafe: routeReady,
    agents: [
      {
        ...deployment.agents[0]!,
        agentId,
        displayName: decision.kind === "unknown" ? agentId : decision.agentLabel,
        enabled: true,
        isDefault: true,
      },
    ],
    nodeReadiness: {
      ...deployment.nodeReadiness,
      source:
        decision.kind === "unknown"
          ? { state: "unknown", reason: decision.reason }
          : { state: "current" },
      agents: [
        decision.kind === "unavailable"
          ? {
              state: "unavailable",
              agentId,
              reason: decision.reason,
            }
          : { state: "ready", agentId },
      ],
    },
  });
}

describe("copyText", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    Object.assign(navigator, { clipboard: undefined });
  });

  it("prefers navigator.clipboard and falls back to execCommand", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    expect(await copyText("hello")).toBe(true);
    expect(writeText).toHaveBeenCalledWith("hello");

    Object.assign(navigator, { clipboard: undefined });
    document.execCommand = vi.fn().mockReturnValue(true);
    const copyButton = document.createElement("button");
    document.body.appendChild(copyButton);
    copyButton.focus();
    expect(await copyText("legacy")).toBe(true);
    expect(document.execCommand).toHaveBeenCalledWith("copy");
    expect(copyButton).toHaveFocus();
    expect(document.body.querySelector("textarea[readonly]")).toBeNull();
    copyButton.remove();
  });

  it("restores focus and removes the fallback textarea when execCommand throws", async () => {
    Object.assign(navigator, { clipboard: undefined });
    document.execCommand = vi.fn(() => {
      throw new Error("clipboard denied");
    });
    const copyButton = document.createElement("button");
    document.body.appendChild(copyButton);
    copyButton.focus();

    expect(await copyText("legacy")).toBe(false);
    expect(copyButton).toHaveFocus();
    expect(document.body.querySelector("textarea[readonly]")).toBeNull();
    copyButton.remove();
  });
});

describe("transcript copy actions", () => {
  afterEach(() => vi.restoreAllMocks());

  it("copies a user message's content", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(
      <MessageList
        timelineItems={[
          userMessage({
            kind: "userMessage",
            reconstruction: { state: "ready" },
            itemKey: "u1",
            content: "copy me please",
          }),
        ]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("copy me please"));
  });

  it("renders a copy button on fenced code blocks", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(
      <MessageList
        timelineItems={[
          assistantMessage({
            kind: "assistantMessage",
            reconstruction: { state: "ready" },
            itemKey: "a1",
            content: "```rust\nfn main() {}\n```",
          }),
        ]}
      />,
    );

    const buttons = screen.getAllByRole("button", { name: "Copy" });
    expect(buttons.length).toBe(2);
    fireEvent.click(buttons[buttons.length - 1]);
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith(expect.stringContaining("fn main")),
    );
  });
});

describe("error card retry", () => {
  const session: DesktopSessionSnapshot = sessionSnapshot({
    sessionId: "s1",
    latestRequestId: "req-failed",
    turnState: "failed",
    retryEligibility: { eligible: true, denialReason: null },
    /* only the failure is read here */
    latestRequestOutcome: {
      failureReason: "provider exploded",
    } as DesktopSessionSnapshot["latestRequestOutcome"],
    timelineItems: [
      userMessage({
        kind: "userMessage",
        reconstruction: { state: "ready" },
        itemKey: "u1",
        content: "the failed ask",
      }),
    ],
  });

  it("summarizes the error, keeps raw text in a disclosure, and retries the failed content", () => {
    const onRetryMessage = vi.fn();
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={session}
        onRetryMessage={onRetryMessage}
      />,
    );

    const card = screen.getByTestId("response-error-card");
    expect(card).toHaveTextContent("couldn't complete this turn");
    expect(card).toHaveTextContent("provider exploded");

    fireEvent.click(screen.getByTestId("retry-turn"));
    expect(onRetryMessage).toHaveBeenCalledWith("req-failed");
  });

  it("disables retry while the selected agent backend is unavailable", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={session}
        onRetryMessage={vi.fn()}
        retryUnavailableHint="Backend “Workstation 2” is still checking readiness"
      />,
    );

    expect(screen.getByTestId("retry-turn")).toBeDisabled();
    expect(screen.getByTestId("retry-turn")).toHaveAttribute(
      "title",
      "Backend “Workstation 2” is still checking readiness",
    );
  });

  it("omits Retry when no handler is wired", () => {
    render(<ChatTranscriptPanel selectedSessionId="s1" session={session} />);
    expect(screen.queryByTestId("retry-turn")).not.toBeInTheDocument();
  });

  it("omits Retry when the persisted predecessor is ineligible", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={{
          ...session,
          retryEligibility: {
            eligible: false,
            denialReason: "nonInteractiveOrigin",
          },
        }}
        onRetryMessage={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("retry-turn")).not.toBeInTheDocument();
  });

  it("disables Retry while the authoritative retry intent is pending", async () => {
    let resolveRetry: (() => void) | undefined;
    const onRetryMessage = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveRetry = resolve;
        }),
    );
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={session}
        onRetryMessage={onRetryMessage}
      />,
    );

    const retry = screen.getByTestId("retry-turn");
    fireEvent.click(retry);
    fireEvent.click(retry);
    expect(onRetryMessage).toHaveBeenCalledTimes(1);
    expect(retry).toBeDisabled();
    expect(retry).toHaveTextContent("Retrying");

    resolveRetry?.();
    await waitFor(() => expect(retry).not.toBeDisabled());
  });

  it("does not present an interrupted turn as a retryable failure", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={{
          ...session,
          turnState: "interrupted",
          latestRequestOutcome: {
            failureReason: "agent stream interrupted",
            cancelCause: {
              cause: "interrupted",
              source: "requestLifecycle",
              confidence: "direct",
              at: "2026-07-25T20:00:00Z",
              evidence: ['AgentRequest.lifecycle_state = "interrupted"'],
            },
          },
        }}
        onRetryMessage={vi.fn()}
      />,
    );

    expect(screen.queryByTestId("response-error-card")).not.toBeInTheDocument();
    expect(screen.queryByTestId("retry-turn")).not.toBeInTheDocument();
    expect(
      screen.getByText(/interrupted/i, { selector: ".cause-badge" }),
    ).toBeInTheDocument();
  });

  it("suppresses Retry as soon as a user-cancel cause is observed", () => {
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={{
          ...session,
          latestRequestOutcome: {
            failureReason: "completion cancelled",
            cancelCause: {
              cause: "userCancelled",
              source: "requestInterrupt",
              confidence: "direct",
              at: "2026-07-25T20:00:00Z",
              evidence: ["AgentRequest.interrupt_requested_at = 2026-07-25T20:00:00Z"],
            },
          },
        }}
        onRetryMessage={vi.fn()}
      />,
    );

    expect(screen.queryByTestId("response-error-card")).not.toBeInTheDocument();
    expect(screen.queryByTestId("retry-turn")).not.toBeInTheDocument();
  });

  it("uses the predecessor-aware retry API when the composer draft is empty", async () => {
    const sendChatMessage = vi.fn();
    const retryRequest = vi.fn().mockResolvedValue({
      nodeDid: deployment.nodeDid,
      sessionId: "s1",
      requestId: "req_retry",
    });
    const api = {
      sendChatMessage,
      retryRequest,
    } as unknown as DesktopApiAdapter;
    const shellProjection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: deployment.nodeDid,
      selectedSessionId: "s1",
      sending: false,
      session,
      selectedSessionSummary: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor(),
    });
    expect(shellProjection.nonEmptyContentSendStatus).toEqual({ kind: "ready" });

    const { actions } = chatActions(api, {
      agentReadiness: readyNodeReadiness,
      shellProjection,
      retryShellProjection: shellProjection,
    });

    actions.retryMessage("req-failed");

    await waitFor(() =>
      expect(retryRequest).toHaveBeenCalledWith("req-failed", deployment.nodeDid),
    );
    expect(sendChatMessage).not.toHaveBeenCalled();
  });

  it("gates Retry with the persisted session agent, not the composer selection", async () => {
    const retryRequest = vi.fn().mockResolvedValue({
      nodeDid: deployment.nodeDid,
      sessionId: "s1",
      requestId: "req_retry",
    });
    const reportFailure = vi.fn();
    const composerProjection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: deployment.nodeDid,
      selectedSessionId: "s1",
      sending: false,
      session,
      selectedSessionSummary: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor(),
    });
    const blockedRetryProjection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: deployment.nodeDid,
      selectedSessionId: "s1",
      sending: false,
      session,
      selectedSessionSummary: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor(unavailableNodeReadiness),
    });

    const api = { retryRequest } as unknown as DesktopApiAdapter;
    const { actions: blocked } = chatActions(
      api,
      {
        agentReadiness: readyNodeReadiness,
        shellProjection: composerProjection,
        retryShellProjection: blockedRetryProjection,
      },
      reportFailure,
    );
    await blocked.retryMessage("req-failed");
    expect(retryRequest).not.toHaveBeenCalled();
    expect(reportFailure).toHaveBeenCalledWith(
      blockedRetryProjection.nonEmptyContentSendStatus.kind === "disabled"
        ? blockedRetryProjection.nonEmptyContentSendStatus.hint
        : null,
    );

    reportFailure.mockClear();
    const { actions: readyRetry } = chatActions(
      api,
      {
        agentReadiness: unavailableNodeReadiness,
        shellProjection: blockedRetryProjection,
        retryShellProjection: composerProjection,
      },
      reportFailure,
    );
    await readyRetry.retryMessage("req-failed");
    expect(retryRequest).toHaveBeenCalledWith("req-failed", deployment.nodeDid);
    expect(reportFailure).not.toHaveBeenCalledWith(
      blockedRetryProjection.nonEmptyContentSendStatus.kind === "disabled"
        ? blockedRetryProjection.nonEmptyContentSendStatus.hint
        : null,
    );
  });

  it("projects an acknowledged send immediately before replication observes it", async () => {
    const sendChatMessage = vi.fn().mockResolvedValue({
      nodeDid: deployment.nodeDid,
      sessionId: "s1",
      requestId: "req_new",
    });
    const shellProjection = projectChatShell({
      clientAvailable: true,
      selectedNodeDid: deployment.nodeDid,
      selectedSessionId: "s1",
      sending: false,
      session,
      selectedSessionSummary: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor(),
    });

    const { actions, stores } = chatActions(
      { sendChatMessage } as unknown as DesktopApiAdapter,
      {
        agentReadiness: readyNodeReadiness,
        shellProjection,
        retryShellProjection: shellProjection,
      },
    );

    await actions.sendMessage("check the upgrade");

    expect(stores.chat.getState().optimisticPendingTurn).toEqual(
      expect.objectContaining({
        sessionId: "s1",
        requestId: "req_new",
        content: "check the upgrade",
        lifecycleState: "pending",
      }),
    );
  });
});
