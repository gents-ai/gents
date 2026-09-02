import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DeploymentView,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { ActiveChatWorkspace } from "../src/components/ChatWorkspace";
import { ConversationLoadingStatus } from "../src/components/ConversationLoadingStatus";

describe("ConversationLoadingStatus", () => {
  it("renders the projected layer and runs its matching recovery", () => {
    const onRetry = vi.fn(async () => {});
    render(
      <ConversationLoadingStatus
        status={{
          layer: "sessionSync",
          phase: "failed",
          title: "Conversation sync failed",
          detail: "The secure transfer did not complete.",
          action: "retryHydration",
        }}
        onRetryHydration={onRetry}
      />,
    );

    expect(screen.getByTestId("conversation-loading-status")).toHaveAttribute(
      "data-loading-layer",
      "sessionSync",
    );
    expect(screen.getByTestId("conversation-loading-status")).toHaveAttribute(
      "role",
      "alert",
    );
    fireEvent.click(screen.getByTestId("conversation-loading-retryHydration"));
    expect(onRetry).toHaveBeenCalledOnce();
  });
});

const deployment = {
  peerId: "peer-1",
  label: "Local",
  agentDid: "did:test:agent",
  addr: "local",
  source: "local",
  graphql: null,
  dialSucceeded: true,
  pairingReady: true,
  pairing: [],
  chatSafe: true,
  routes: [],
  lastError: null,
  defaultBehaviorId: "default",
  agentPrincipal: { agentDid: "did:test:agent", displayName: "Local" },
  runtime: null,
  behaviors: [
    { behaviorId: "default", displayName: "Default", enabled: true, isDefault: true },
  ],
  behaviorEnvironments: [],
  inferenceBackends: [],
  inferenceProfiles: [],
  toolSelections: [],
  toolServiceRegistries: [],
  skills: [],
  tasks: [],
  schedules: [],
  eventTriggers: [],
  conversations: [],
  mailboxItems: [],
} as unknown as DeploymentView;

function workspace(
  session: DesktopSessionSnapshot | null,
  selectedSessionId = "session-1",
) {
  return (
    <ActiveChatWorkspace
      activeRequestId={null}
      approxSerializedBytes={0}
      canSend
      configuredPeerCount={1}
      conversationLoadingStatus={{
        layer: "sessionSync",
        phase: "loading",
        title: "Syncing conversation history",
        detail: "Fetching session history · 2 of 6",
        action: null,
      }}
      activityStatus={null}
      dialedPeerCount={1}
      draft=""
      interruptVisible={false}
      onDraftChange={vi.fn()}
      onRenameConversationTitle={vi.fn()}
      onSend={vi.fn()}
      rowCount={1}
      runtimeHealth={null}
      selectedBehaviorId="default"
      selectedConversationTitle="Other device"
      selectedDeployment={deployment}
      selectedSessionId={selectedSessionId}
      sending={false}
      session={session}
      turnState={null}
    />
  );
}

describe("ChatWorkspace loading", () => {
  it("keeps already-local transcript visible while history hydrates", () => {
    const session = {
      sessionId: "session-1",
      agentDid: "did:test:agent",
      timelineItems: [
        {
          kind: "userMessage",
          itemKey: "u1",
          requestId: "req-1",
          content: "hello from the other device",
        },
      ],
    } as unknown as DesktopSessionSnapshot;

    render(workspace(session));
    expect(screen.getByTestId("conversation-loading-status")).toHaveTextContent(
      "Fetching session history · 2 of 6",
    );
    expect(screen.getByText("hello from the other device")).toBeInTheDocument();
  });

  it("never displays a stale conversation while the selected one loads", () => {
    const stale = {
      sessionId: "session-old",
      agentDid: "did:test:agent",
      timelineItems: [
        {
          kind: "userMessage",
          itemKey: "old",
          requestId: "req-old",
          content: "must not leak into the next conversation",
        },
      ],
    } as unknown as DesktopSessionSnapshot;

    render(workspace(stale));
    expect(
      screen.queryByText("must not leak into the next conversation"),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("transcript-loading")).toBeInTheDocument();
  });
});
