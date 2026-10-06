/* A node pick in local storage outlives the node it names: storage belongs
   to the app's origin, so a reset home or a removed peer leaves a DID no
   node has. The list must not narrow to nothing, and an empty list must
   not claim there are no sessions when a filter hides them. */
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";

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
const node = (agentDid: string, sessions: SessionSummary[]) => ({
  agentDid,
  label: agentDid,
  agentPrincipal: { displayName: null },
  source: "enrollment",
  sessions,
  mailboxItems: [],
  behaviors: [],
  behaviorConfigs: [],
  behaviorEnvironments: [],
});
const shell = (deployments: ReturnType<typeof node>[]) =>
  ({
    holds: [],
    behaviorDescriptions: {},
    deployments,
    selectedDeployment: deployments[0] ?? null,
    selectedAgentDid: deployments[0]?.agentDid ?? null,
    snapshot: { bootstrap: { initAgentDid: "did:key:home" } },
  }) as unknown as Shell;
const storedNodes = (dids: string[]) =>
  vi.stubGlobal("localStorage", {
    getItem: (key: string) =>
      key === "gents-prototype-sessions-nodes" ? JSON.stringify(dids) : null,
    setItem: vi.fn(),
    removeItem: vi.fn(),
  });

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("a stored node pick", () => {
  it("naming only a node that is gone shows the working node's sessions", () => {
    storedNodes(["did:key:previous-home"]);
    render(<SessionsScreen shell={shell([node("did:key:home", [session({})])])} />);
    expect(screen.getByText("Greeting")).toBeInTheDocument();
    expect(screen.queryByText("No sessions yet")).toBeNull();
  });

  it("that hides every session says nothing matches, not that there are none", () => {
    storedNodes(["did:key:peer"]);
    render(
      <SessionsScreen
        shell={shell([node("did:key:home", [session({})]), node("did:key:peer", [])])}
      />,
    );
    expect(screen.queryByText("Greeting")).toBeNull();
    expect(screen.getByText("No sessions match.")).toBeInTheDocument();
    expect(screen.queryByText("No sessions yet")).toBeNull();
  });

  it("leaves a fresh client with no sessions anywhere saying there are none yet", () => {
    storedNodes([]);
    render(<SessionsScreen shell={shell([node("did:key:home", [])])} />);
    expect(screen.getByText("No sessions yet")).toBeInTheDocument();
  });
});
