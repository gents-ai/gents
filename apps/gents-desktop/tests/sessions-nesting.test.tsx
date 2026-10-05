import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { emptyFilter, filterSessions } from "../src/ui/screens/SessionFilters";
import { fleetFor } from "./shell-fixture";

const session = (overrides: Partial<SessionSummary>): SessionSummary => ({
  sessionId: "parent",
  agentDid: "did:test:owner",
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: "physical-parent-1",
  latestRequestId: "logical-parent-1",
  closedAt: null,
  tags: [],
  provenance: null,
  title: "Lead",
  previewText: null,
  status: "processing",
  behaviorId: null,
  taskId: null,
  taskName: null,
  triggerId: null,
  triggerKind: null,
  createdAt: null,
  updatedAt: null,
  turnState: "running",
  messageCount: null,
  toolCallCount: null,
  ...overrides,
});
const parent = session({});
const child = session({
  sessionId: "child",
  title: "Worker",
  latestRequestDocId: "physical-child",
  latestRequestId: "logical-child",
  provenance: {
    parent_request_doc_id: "physical-parent-1",
    task_id: null,
    graph_run_id: null,
    fork: null,
  },
  startedBy: {
    sessionId: "parent",
    agentDid: "did:test:owner",
    requesterDid: null,
    causeRequestDocId: "physical-parent-1",
  },
});
const shell = (sessions: SessionSummary[]) =>
  ({
    behaviorDescriptions: {},
    deployments: [deploymentWith(sessions)],
    fleet: fleetFor([deploymentWith(sessions)]),
    selectedDeployment: deploymentWith(sessions),
  }) as unknown as Shell;
const deploymentWith = (sessions: SessionSummary[]) => ({
  agentDid: "did:key:node",
  sessions,
  mailboxItems: [],
  behaviors: [],
  behaviorConfigs: [],
  behaviorEnvironments: [],
});

beforeEach(() =>
  vi.stubGlobal("localStorage", {
    getItem: () => null,
    setItem: vi.fn(),
    removeItem: vi.fn(),
  }),
);
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("sessions started by another session", () => {
  it("nests by session identity when physical and logical request IDs differ, including after the parent advances", () => {
    const { rerender } = render(<SessionsScreen shell={shell([child, parent])} />);
    expect(screen.queryByTestId("session-child")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /1 more worker/ }));
    expect(screen.getByTestId("session-child")).toHaveClass("pl-9");
    rerender(
      <SessionsScreen
        shell={shell([
          child,
          session({
            latestRequestDocId: "physical-parent-2",
            latestRequestId: "logical-parent-2",
          }),
        ])}
      />,
    );
    expect(screen.getByTestId("session-child")).toHaveClass("pl-9");
    expect(screen.getByRole("button", { name: /1 worker/ })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
  });

  it("does not nest under an identical session label in a different requester scope", () => {
    render(
      <SessionsScreen
        shell={shell([child, session({ requesterDid: "did:test:other" })])}
      />,
    );
    expect(screen.getByTestId("session-child")).not.toHaveClass("pl-9");
  });

  it("filters by the resolved starting session", () => {
    const unresolved = session({
      sessionId: "unresolved",
      provenance: child.provenance,
    });
    const sessions = [parent, child, unresolved];
    expect(
      filterSessions(sessions, { ...emptyFilter, sources: ["session"] }, null),
    ).toEqual([child]);
    expect(
      filterSessions(sessions, { ...emptyFilter, sources: ["person"] }, null),
    ).toEqual([parent, unresolved]);
  });
});
