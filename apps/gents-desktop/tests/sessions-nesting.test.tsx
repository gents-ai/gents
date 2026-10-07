import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { emptyFilter } from "../src/ui/lib/session-filter";
import { filterSessions } from "../src/ui/screens/SessionFilters";
import { node, publish, testApp, withApp } from "./app-fixture";

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
const deploymentWith = (sessions: SessionSummary[]) =>
  node({
    agentDid: "did:key:node",
    sessions,
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
    const app = testApp({ deployments: [deploymentWith([child, parent])] });
    const { rerender } = render(<SessionsScreen />, {
      wrapper: withApp(app),
    });
    expect(screen.queryByTestId("session-child")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /1 more worker/ }));
    expect(screen.getByTestId("session-child")).toHaveClass("pl-9");
    const advanced = [
      child,
      session({
        latestRequestDocId: "physical-parent-2",
        latestRequestId: "logical-parent-2",
      }),
    ];
    act(() => publish(app, [deploymentWith(advanced)]));
    rerender(<SessionsScreen />);
    expect(screen.getByTestId("session-child")).toHaveClass("pl-9");
    expect(screen.getByRole("button", { name: /1 worker/ })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
  });

  it("does not nest under an identical session label in a different requester scope", () => {
    const sessions = [child, session({ requesterDid: "did:test:other" })];
    render(<SessionsScreen />, {
      wrapper: withApp(testApp({ deployments: [deploymentWith(sessions)] })),
    });
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
