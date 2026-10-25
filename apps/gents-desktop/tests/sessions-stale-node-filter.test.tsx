import { cleanup, fireEvent, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { listViews } from "../src/ui/app/listViews";
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
const storedNodes = (dids: string[]) => listViews.setSessionNodes(dids);

afterEach(() => {
  cleanup();
  listViews.setSessionNodes(null);
});

describe("a stored node pick", () => {
  it("naming only a node that is gone shows every node's sessions", () => {
    storedNodes(["did:key:previous-home"]);
    renderIn(app([nodeWith("did:key:home", [session({})])]), <SessionsScreen />);
    expect(screen.getByText("Greeting")).toBeInTheDocument();
    expect(screen.queryByText("No sessions yet")).toBeNull();
  });

  it("of every node is where the list starts, so it does not read as narrowed", () => {
    storedNodes([]);
    renderIn(
      app([nodeWith("did:key:home", [session({})]), nodeWith("did:key:peer", [])]),
      <SessionsScreen />,
    );
    expect(screen.getByText("Greeting")).toBeInTheDocument();
    expect(screen.queryByTestId("sessions-narrowed")).toBeNull();
  });

  it("cleared, shows every node as All nodes, not each node picked", async () => {
    storedNodes(["did:key:peer"]);
    renderIn(
      app([nodeWith("did:key:home", [session({})]), nodeWith("did:key:peer", [])]),
      <SessionsScreen />,
    );
    expect(screen.getByRole("button", { name: "Node" })).not.toHaveTextContent(
      "All nodes",
    );
    fireEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(screen.getByRole("button", { name: "Node" })).toHaveTextContent("All nodes");
    expect(screen.queryByTestId("sessions-narrowed")).toBeNull();
  });

  it("that hides every session says nothing matches, not that there are none", () => {
    storedNodes(["did:key:peer"]);
    renderIn(
      app([nodeWith("did:key:home", [session({})]), nodeWith("did:key:peer", [])]),
      <SessionsScreen />,
    );
    expect(screen.queryByText("Greeting")).toBeNull();
    expect(screen.getByText("No sessions match.")).toBeInTheDocument();
    expect(screen.queryByText("No sessions yet")).toBeNull();
  });

  it("leaves a fresh client with no sessions anywhere saying there are none yet", () => {
    storedNodes([]);
    renderIn(app([nodeWith("did:key:home", [])]), <SessionsScreen />);
    expect(screen.getByText("No sessions yet")).toBeInTheDocument();
  });
});
