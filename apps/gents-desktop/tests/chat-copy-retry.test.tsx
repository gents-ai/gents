import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChatTranscriptPanel } from "@source-inc/gents-desktop-chat";
import { MessageList } from "@source-inc/gents-desktop-chat";
import { createDesktopShellChatActions } from "../src/hooks/desktopShellChatActions";
import {
  getDesktopApiAdapter,
  projectDeploymentOperationalState,
  setDesktopApiAdapterForTests,
  type BehaviorReadinessDecision,
  type DesktopApiAdapter,
} from "@source-inc/gents-desktop-client";
import { projectChatShell } from "@source-inc/gents-desktop-chat";
import { copyText } from "@source-inc/gents-desktop-ui";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { deployment } from "./config-panel-wiring/fixtures";

const readyBehaviorReadiness = {
  kind: "ready",
  behaviorId: "default",
  behaviorLabel: "Default",
} as const;
const unavailableBehaviorReadiness = {
  kind: "unavailable",
  behaviorId: "ops",
  behaviorLabel: "Ops",
  reason: "backend_temporarily_unavailable",
} as const;

function operationalStateFor(
  decision: BehaviorReadinessDecision = readyBehaviorReadiness,
  routeReady = deployment.chatSafe,
) {
  const behaviorId = decision.behaviorId ?? "default";
  return projectDeploymentOperationalState({
    ...deployment,
    pairingReady: routeReady,
    chatSafe: routeReady,
    behaviors: [
      {
        behaviorId,
        displayName: decision.kind === "unknown" ? behaviorId : decision.behaviorLabel,
        enabled: true,
        isDefault: true,
      },
    ],
    behaviorReadiness: {
      ...deployment.behaviorReadiness,
      source:
        decision.kind === "unknown"
          ? { state: "unknown", reason: decision.reason }
          : { state: "current" },
      defaultBehaviorId: behaviorId,
      behaviors: [
        decision.kind === "unavailable"
          ? {
              state: "unavailable",
              behaviorId,
              reason: decision.reason,
            }
          : { state: "ready", behaviorId },
      ],
    },
  });
}

afterEach(() => setDesktopApiAdapterForTests(null));

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
          {
            kind: "userMessage",
            itemKey: "u1",
            content: "copy me please",
          },
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
          {
            kind: "assistantMessage",
            itemKey: "a1",
            content: "```rust\nfn main() {}\n```",
          },
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
  const session: DesktopSessionSnapshot = {
    sessionId: "s1",
    latestRequestId: "req-failed",
    turnState: "failed",
    retryEligibility: { eligible: true, denialReason: null },
    latestResponse: { status: "failed", errorMessage: "provider exploded" },
    timelineItems: [
      {
        kind: "userMessage",
        itemKey: "u1",
        content: "the failed ask",
      },
    ],
  };

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

  it("disables retry while the selected behavior backend is unavailable", () => {
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
          latestResponse: {
            status: "interrupted",
            errorMessage: "agent stream interrupted",
            interruptedAt: "2026-07-25T20:00:00Z",
            cancelCause: {
              cause: "interrupted",
              source: "responseInterruptedAt",
              confidence: "direct",
              at: "2026-07-25T20:00:00Z",
              evidence: ["AgentResponse.interrupted_at = 2026-07-25T20:00:00Z"],
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
          latestResponse: {
            status: "failed",
            errorMessage: "completion cancelled",
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
      agentDid: deployment.agentDid,
      sessionId: "s1",
      requestId: "req_retry",
    });
    setDesktopApiAdapterForTests({
      sendChatMessage,
      retryRequest,
    } as DesktopApiAdapter);
    const shellProjection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: deployment.agentDid,
      selectedSessionId: "s1",
      draft: "",
      sending: false,
      session,
      selectedConversation: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor(),
    });
    expect(shellProjection.sendStatus).toMatchObject({
      kind: "disabled",
      reason: "composerEmpty",
    });
    expect(shellProjection.nonEmptyContentSendStatus).toEqual({ kind: "ready" });

    const actions = createDesktopShellChatActions({
      api: getDesktopApiAdapter(),
      draft: "",
      behaviorReadiness: readyBehaviorReadiness,
      newConversationAgentRef: { current: null },
      refreshSession: vi.fn(),
      refreshSnapshot: vi.fn(),
      selectedDeployment: deployment,
      selectedSessionId: "s1",
      pendingMailboxCauseId: null,
      session,
      setDraft: vi.fn(),
      setError: vi.fn(),
      setLocalWorkflow: vi.fn(),
      setOptimisticPendingTurn: vi.fn(),
      setSelectedBehaviorId: vi.fn(),
      setSelectedSessionId: vi.fn(),
      setSending: vi.fn(),
      setPendingMailboxCauseId: vi.fn(),
      setSession: vi.fn(),
      shellProjection,
      retryShellProjection: shellProjection,
    });

    actions.onRetryMessage("req-failed");

    await waitFor(() => expect(retryRequest).toHaveBeenCalledWith("req-failed"));
    expect(sendChatMessage).not.toHaveBeenCalled();
  });

  it("gates Retry with the persisted session behavior, not the composer selection", async () => {
    const retryRequest = vi.fn().mockResolvedValue({
      agentDid: deployment.agentDid,
      sessionId: "s1",
      requestId: "req_retry",
    });
    const setError = vi.fn();
    const composerProjection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: deployment.agentDid,
      selectedSessionId: "s1",
      draft: "retry",
      sending: false,
      session,
      selectedConversation: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor(),
    });
    const blockedRetryProjection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: deployment.agentDid,
      selectedSessionId: "s1",
      draft: "retry",
      sending: false,
      session,
      selectedConversation: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor(unavailableBehaviorReadiness),
    });
    const common = {
      api: { retryRequest } as unknown as DesktopApiAdapter,
      draft: "",
      newConversationAgentRef: { current: null },
      refreshSession: vi.fn(),
      refreshSnapshot: vi.fn(),
      selectedDeployment: deployment,
      selectedSessionId: "s1",
      pendingMailboxCauseId: null,
      session,
      setDraft: vi.fn(),
      setError,
      setLocalWorkflow: vi.fn(),
      setOptimisticPendingTurn: vi.fn(),
      setSelectedBehaviorId: vi.fn(),
      setSelectedSessionId: vi.fn(),
      setSending: vi.fn(),
      setPendingMailboxCauseId: vi.fn(),
      setSession: vi.fn(),
    };

    const blocked = createDesktopShellChatActions({
      ...common,
      behaviorReadiness: readyBehaviorReadiness,
      shellProjection: composerProjection,
      retryShellProjection: blockedRetryProjection,
    });
    await blocked.onRetryMessage("req-failed");
    expect(retryRequest).not.toHaveBeenCalled();
    expect(setError).toHaveBeenCalledWith(
      blockedRetryProjection.nonEmptyContentSendStatus.kind === "disabled"
        ? blockedRetryProjection.nonEmptyContentSendStatus.hint
        : null,
    );

    setError.mockClear();
    const readyRetry = createDesktopShellChatActions({
      ...common,
      behaviorReadiness: unavailableBehaviorReadiness,
      shellProjection: blockedRetryProjection,
      retryShellProjection: composerProjection,
    });
    await readyRetry.onRetryMessage("req-failed");
    expect(retryRequest).toHaveBeenCalledWith("req-failed");
    expect(setError).not.toHaveBeenCalledWith(
      blockedRetryProjection.nonEmptyContentSendStatus.kind === "disabled"
        ? blockedRetryProjection.nonEmptyContentSendStatus.hint
        : null,
    );
  });

  it("projects an acknowledged send immediately before replication observes it", async () => {
    const sendChatMessage = vi.fn().mockResolvedValue({
      agentDid: deployment.agentDid,
      sessionId: "s1",
      requestId: "req_new",
    });
    const setOptimisticPendingTurn = vi.fn();
    const setDraft = vi.fn();
    const shellProjection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: deployment.agentDid,
      selectedSessionId: "s1",
      draft: "check the upgrade",
      sending: false,
      session,
      selectedConversation: null,
      localWorkflow: { kind: "ready" },
      operationalState: operationalStateFor(),
    });

    const actions = createDesktopShellChatActions({
      api: { sendChatMessage } as unknown as DesktopApiAdapter,
      behaviorReadiness: readyBehaviorReadiness,
      draft: "check the upgrade",
      newConversationAgentRef: { current: null },
      refreshSession: vi.fn(),
      refreshSnapshot: vi.fn(),
      selectedDeployment: deployment,
      selectedSessionId: "s1",
      pendingMailboxCauseId: null,
      session,
      setDraft,
      setError: vi.fn(),
      setLocalWorkflow: vi.fn(),
      setOptimisticPendingTurn,
      setSelectedBehaviorId: vi.fn(),
      setSelectedSessionId: vi.fn(),
      setSending: vi.fn(),
      setPendingMailboxCauseId: vi.fn(),
      setSession: vi.fn(),
      shellProjection,
      retryShellProjection: shellProjection,
    });

    await actions.onSendMessage({ preventDefault: vi.fn() } as never);

    expect(setOptimisticPendingTurn).toHaveBeenCalledWith(
      expect.objectContaining({
        sessionId: "s1",
        requestId: "req_new",
        content: "check the upgrade",
        lifecycleState: "pending",
      }),
    );
    expect(setDraft).toHaveBeenCalledWith("");
  });
});
