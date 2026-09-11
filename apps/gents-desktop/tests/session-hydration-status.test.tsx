import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DeploymentView,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { ActiveChatWorkspace } from "../src/components/ChatWorkspace";
import { SessionLoadingStatus } from "../src/components/SessionLoadingStatus";

describe("SessionLoadingStatus", () => {
  it("renders the projected layer and runs its matching recovery", () => {
    const onRetry = vi.fn(async () => {});
    render(
      <SessionLoadingStatus
        status={{
          layer: "sessionSync",
          phase: "failed",
          title: "Session sync failed",
          detail: "The secure transfer did not complete.",
          action: "retryHydration",
        }}
        onRetryHydration={onRetry}
      />,
    );

    expect(screen.getByTestId("session-loading-status")).toHaveAttribute(
      "data-loading-layer",
      "sessionSync",
    );
    expect(screen.getByTestId("session-loading-status")).toHaveAttribute(
      "role",
      "alert",
    );
    fireEvent.click(screen.getByTestId("session-loading-retryHydration"));
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
  pairing: [],
  chatSafe: true,
  routes: [],
  lastError: null,
  agentPrincipal: { agentDid: "did:test:agent", displayName: "Local" },
  runtime: null,
  behaviors: [
    { behaviorId: "default", displayName: "Default", enabled: true, isDefault: true },
  ],
  behaviorEnvironments: [],
  inferenceBackends: [],
  inferenceProfiles: [],
  toolServiceRegistries: [],
  skills: [],
  tasks: [],
  schedules: [],
  eventSources: [],
  triggers: [],
  sessions: [],
  mailboxItems: [],
} as unknown as DeploymentView;

const api = {
  previewInterruptCascade: vi.fn(async () => ({
    rootRequestId: "",
    requests: [],
    total: 0,
  })),
  interruptRequest: vi.fn(async () => undefined),
} as unknown as DesktopApiAdapter;

function workspace(
  session: DesktopSessionSnapshot | null,
  selectedSessionId = "session-1",
) {
  return (
    <ActiveChatWorkspace
      api={api}
      activeRequestId={null}
      approxSerializedBytes={0}
      canSend
      sessionLoadingStatus={{
        layer: "sessionSync",
        phase: "loading",
        title: "Syncing session history",
        detail: "Fetching session history · 2 of 6",
        action: null,
      }}
      activityStatus={null}
      draft=""
      interruptVisible={false}
      onDraftChange={vi.fn()}
      onRenameSessionTitle={vi.fn()}
      onSend={vi.fn()}
      rowCount={1}
      runtimeHealth={null}
      selectedBehaviorId="default"
      selectedSessionSummaryTitle="Other device"
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
    expect(screen.getByTestId("session-loading-status")).toHaveTextContent(
      "Fetching session history · 2 of 6",
    );
    expect(screen.getByText("hello from the other device")).toBeInTheDocument();
  });

  it("never displays a stale session while the selected one loads", () => {
    const stale = {
      sessionId: "session-old",
      agentDid: "did:test:agent",
      timelineItems: [
        {
          kind: "userMessage",
          itemKey: "old",
          requestId: "req-old",
          content: "must not leak into the next session",
        },
      ],
    } as unknown as DesktopSessionSnapshot;

    render(workspace(stale));
    expect(
      screen.queryByText("must not leak into the next session"),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("transcript-loading")).toBeInTheDocument();
  });
});
