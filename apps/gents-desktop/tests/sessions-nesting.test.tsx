import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { emptyFilter } from "../src/ui/lib/session-filter";
import { matchesView } from "../src/ui/screens/SessionFilters";
import { listViews, resetListViews } from "../src/ui/app/listViews";
import { node, publish, testApp, withApp } from "./app-fixture";

const session = (overrides: Partial<SessionSummary>): SessionSummary => ({
  sessionId: "parent",
  nodeDid: "did:test:owner",
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
  agentId: null,
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
    fork: null,
  },
  startedBy: {
    sessionId: "parent",
    nodeDid: "did:test:owner",
    requesterDid: null,
    causeRequestDocId: "physical-parent-1",
  },
});
const deploymentWith = (sessions: SessionSummary[]) =>
  node({
    nodeDid: "did:key:node",
    sessions,
    behaviors: [],
    agentConfigs: [],
    behaviorEnvironments: [],
  });

beforeEach(() => {
  vi.stubGlobal("localStorage", {
    getItem: () => null,
    setItem: vi.fn(),
    removeItem: vi.fn(),
  });
  resetListViews();
});
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
    fireEvent.click(screen.getByRole("button", { name: /1 started session/ }));
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
    expect(screen.getByRole("button", { name: /1 started session/ })).toHaveAttribute(
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

  it("keeps the toggle once the started work has finished, closed until opened, and remembers it", () => {
    const finished = { ...child, turnState: "completed" };
    const show = () =>
      render(<SessionsScreen />, {
        wrapper: withApp(
          testApp({ deployments: [deploymentWith([finished, parent])] }),
        ),
      });
    show();
    const toggle = screen.getByRole("button", { name: /1 started session/ });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByTestId("session-child")).toBeNull();
    fireEvent.click(toggle);
    expect(screen.getByTestId("session-child")).toBeInTheDocument();
    cleanup();
    show();
    expect(screen.getByTestId("session-child")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /1 started session/ })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
  });

  it("says how far a filter narrows the list, keeps the parent as context, and clears", () => {
    const failed = { ...child, turnState: "failed" };
    act(() => listViews.setSessionFilter({ ...emptyFilter, states: ["failed"] }));
    render(<SessionsScreen />, {
      wrapper: withApp(testApp({ deployments: [deploymentWith([failed, parent])] })),
    });
    expect(screen.getByTestId("sessions-narrowed")).toHaveTextContent(
      "1 of 2 sessions",
    );
    expect(screen.getByTestId("session-parent")).toHaveClass("opacity-60");
    expect(screen.getByRole("button", { name: /1 started session/ })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    fireEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(screen.queryByTestId("sessions-narrowed")).toBeNull();
    expect(screen.getByTestId("session-parent")).not.toHaveClass("opacity-60");
  });

  it("nests a session under its line's top through a starter on a node the list leaves out", () => {
    const on = (nodeDid: string, sessionId: string, by?: SessionSummary) =>
      session({
        sessionId,
        title: sessionId,
        nodeDid,
        latestRequestDocId: `doc-${sessionId}`,
        latestRequestId: `req-${sessionId}`,
        startedBy: by
          ? {
              sessionId: by.sessionId,
              nodeDid: by.nodeDid,
              requesterDid: null,
              causeRequestDocId: `doc-${by.sessionId}`,
            }
          : null,
      });
    const lead = on("did:key:a", "lead");
    const worker = on("did:key:b", "worker", lead);
    const helper = on("did:key:a", "helper", worker);
    act(() => listViews.setSessionNodes(["did:key:a"]));
    render(<SessionsScreen />, {
      wrapper: withApp(
        testApp({
          deployments: [
            node({ nodeDid: "did:key:a", sessions: [lead, helper] }),
            node({ nodeDid: "did:key:b", sessions: [worker] }),
          ],
        }),
      ),
    });
    expect(screen.queryByTestId("session-helper")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /1 started session/ }));
    expect(screen.getByTestId("session-helper")).toHaveClass("pl-9");
    expect(screen.queryByTestId("session-worker")).toBeNull();
  });

  it("filters by the resolved starting session", () => {
    const unresolved = session({
      sessionId: "unresolved",
      provenance: child.provenance,
    });
    const sessions = [parent, child, unresolved];
    const matching = (sources: ("session" | "person")[]) =>
      sessions.filter((c) => matchesView(c, { ...emptyFilter, sources }, null));
    expect(matching(["session"])).toEqual([child]);
    expect(matching(["person"])).toEqual([parent, unresolved]);
  });
});
