import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SessionHydrationStatus } from "../src/components/SessionHydrationStatus";
import { ActiveChatWorkspace } from "../src/components/ChatWorkspace";
import type {
  DeploymentView,
  DesktopSessionSnapshot,
  SessionHydrationView,
} from "@source-inc/gents-desktop-client";

function hydration(
  overrides: Partial<SessionHydrationView> = {},
): SessionHydrationView {
  return {
    sessionId: "session-1",
    agentDid: "did:test:agent",
    phase: "serving",
    mergedCount: 2,
    servedCount: 6,
    ...overrides,
  };
}

describe("SessionHydrationStatus", () => {
  it("renders requested, serving counts, complete, and failed retry", async () => {
    const { rerender } = render(
      <SessionHydrationStatus
        hydration={hydration({ phase: "requested", mergedCount: 0, servedCount: null })}
        sessionId="session-1"
      />,
    );
    expect(screen.getByTestId("session-hydration-status")).toHaveAttribute(
      "data-hydration-phase",
      "requested",
    );
    expect(screen.getByTestId("session-hydration-status")).toHaveTextContent(
      "Fetching session history",
    );

    rerender(<SessionHydrationStatus hydration={hydration()} sessionId="session-1" />);
    expect(screen.getByTestId("session-hydration-status")).toHaveTextContent(
      "Fetching session history · 2 of 6",
    );

    rerender(
      <SessionHydrationStatus
        hydration={hydration({ phase: "complete", mergedCount: 6 })}
        sessionId="session-1"
      />,
    );
    expect(screen.getByTestId("session-hydration-status")).toHaveTextContent(
      "Session history loaded · 6 of 6",
    );
    expect(screen.queryByTestId("session-hydration-retry")).not.toBeInTheDocument();

    const onRetry = vi.fn(async () => {});
    rerender(
      <SessionHydrationStatus
        hydration={hydration({ phase: "failed" })}
        onRetry={onRetry}
        sessionId="session-1"
      />,
    );
    expect(screen.getByTestId("session-hydration-status")).toHaveAttribute(
      "role",
      "alert",
    );
    fireEvent.click(screen.getByTestId("session-hydration-retry"));
    expect(onRetry).toHaveBeenCalledOnce();
  });

  it("does not render another session's hydration onto the selected session", () => {
    render(
      <SessionHydrationStatus
        hydration={hydration({ sessionId: "session-2" })}
        sessionId="session-1"
      />,
    );
    expect(screen.queryByTestId("session-hydration-status")).not.toBeInTheDocument();
  });
});

describe("ChatWorkspace hydration", () => {
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
      hydration: hydration(),
    } as unknown as DesktopSessionSnapshot;

    render(
      <ActiveChatWorkspace
        activeRequestId={null}
        approxSerializedBytes={0}
        canSend
        configuredPeerCount={1}
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
        selectedSessionId="session-1"
        sending={false}
        sendHint={null}
        session={session}
        turnState={null}
      />,
    );

    expect(screen.getByTestId("session-hydration-status")).toHaveTextContent(
      "Fetching session history · 2 of 6",
    );
    expect(screen.getByText("hello from the other device")).toBeInTheDocument();
  });
});
