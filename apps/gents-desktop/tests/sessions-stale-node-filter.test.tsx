import { act, cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { resetListViews } from "../src/ui/app/listViews";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { node, renderIn, testApp } from "./app-fixture";

const session = (overrides: Partial<SessionSummary>): SessionSummary => ({
  sessionId: "s1",
  nodeDid: "did:key:home",
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: "physical-1",
  latestRequestId: "logical-1",
  closedAt: null,
  archivedAt: null,
  archiveRevision: "0",
  tags: [],
  provenance: null,
  title: "Greeting",
  previewText: null,
  status: "completed",
  agentId: null,
  taskId: null,
  taskName: null,
  triggerId: null,
  triggerKind: null,
  createdAt: null,
  updatedAt: null,
  turnState: "idle",
  messageCount: null,
  toolCallCount: null,
  ...overrides,
});
const nodeWith = (nodeDid: string, sessions: SessionSummary[]) =>
  node({ nodeDid, label: nodeDid, source: "enrollment", sessions });

const app = (deployments: ReturnType<typeof nodeWith>[]) =>
  testApp({
    snapshot: {
      bootstrap: { initNodeDid: "did:key:home" },
      client: { deployments },
    },
  });

afterEach(() => {
  cleanup();
  resetListViews();
});

describe("the active node's sessions", () => {
  const deployments = () => [
    nodeWith("did:key:home", [session({})]),
    nodeWith("did:key:peer", [
      session({
        sessionId: "peer-session",
        nodeDid: "did:key:peer",
        title: "Remote work",
      }),
    ]),
    nodeWith("did:key:quiet", []),
  ];

  it("switches the list with the window selection without a separate node filter", () => {
    const client = app(deployments());
    renderIn(client, <SessionsScreen />);
    expect(screen.getByText("Greeting")).toBeInTheDocument();
    expect(screen.queryByText("Remote work")).toBeNull();
    act(() => client.actions.selectNode("did:key:peer"));
    expect(screen.queryByText("Greeting")).toBeNull();
    expect(screen.getByText("Remote work")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Node" })).toBeNull();
    expect(screen.queryByTestId("sessions-narrowed")).toBeNull();
  });

  it("does not substitute local sessions when the active node is missing", () => {
    const client = app(deployments());
    client.actions.selectNode("did:key:missing");
    renderIn(client, <SessionsScreen />);
    expect(screen.queryByText("Greeting")).toBeNull();
    expect(screen.queryByText("Remote work")).toBeNull();
  });

  it("shows an empty active node without leaking another node's sessions", () => {
    const client = app(deployments());
    client.actions.selectNode("did:key:quiet");
    renderIn(client, <SessionsScreen />);
    expect(screen.getByText("No sessions yet")).toBeInTheDocument();
    expect(screen.queryByText("Greeting")).toBeNull();
    expect(screen.queryByRole("button", { name: "Clear filters" })).toBeNull();
  });
});
