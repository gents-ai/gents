/* A node pick kept in browser storage outlives the node it names: storage belongs
   to the app's origin, so a reset home or a removed peer leaves a DID no
   node has. The list must not narrow to nothing, and an empty list must
   not claim there are no sessions when a filter hides them. */
import { cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { listViews } from "../src/ui/app/listViews";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { node, renderIn, testApp } from "./app-fixture";

const session = (overrides: Partial<SessionSummary>): SessionSummary => ({
  sessionId: "s1",
  agentDid: "did:key:home",
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: "physical-1",
  latestRequestId: "logical-1",
  closedAt: null,
  tags: [],
  provenance: null,
  title: "Greeting",
  previewText: null,
  status: "completed",
  behaviorId: null,
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
const nodeWith = (agentDid: string, sessions: SessionSummary[]) =>
  node({ agentDid, label: agentDid, source: "enrollment", sessions });
/* the home is the working node, where the list starts */
const app = (deployments: ReturnType<typeof nodeWith>[]) =>
  testApp({
    snapshot: {
      bootstrap: { initAgentDid: "did:key:home" },
      client: { deployments },
    },
  });
const storedNodes = (dids: string[]) => listViews.setSessionNodes(dids);

afterEach(() => {
  cleanup();
  listViews.setSessionNodes(null);
});

describe("a stored node pick", () => {
  it("naming only a node that is gone shows the working node's sessions", () => {
    storedNodes(["did:key:previous-home"]);
    renderIn(app([nodeWith("did:key:home", [session({})])]), <SessionsScreen />);
    expect(screen.getByText("Greeting")).toBeInTheDocument();
    expect(screen.queryByText("No sessions yet")).toBeNull();
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
