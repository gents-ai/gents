import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { emptyFilter } from "../src/ui/lib/session-filter";
import { matchesView } from "../src/ui/screens/SessionFilters";
import { listViews, resetListViews } from "../src/ui/app/listViews";
import { sessionKeyOf } from "../src/hooks/fleetStore";
import type { Route } from "../src/ui/lib/router";
import { OPENED_SHOWN } from "../src/ui/lib/session-rows";
import { node, publish, renderRouted, testApp } from "./app-fixture";

const session = (overrides: Partial<SessionSummary>): SessionSummary => ({
  sessionId: "parent",
  nodeDid: "did:test:owner",
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: "physical-parent-1",
  latestRequestId: "logical-parent-1",
  closedAt: null,
  archivedAt: null,
  archiveRevision: "0",
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
const depthOf = (testId: string) =>
  screen.getByTestId(testId).closest("li")!.dataset.depth;
const toggle = (sessionId: string) =>
  screen.getByTestId(`sessions-toggle-${sessionId}`);
const page = (route: Route) =>
  route.name === "sessions" ? (
    <SessionsScreen nodeDid={route.nodeDid} entry={route} />
  ) : route.name === "sessionsUnder" ? (
    <SessionsScreen under={route.session} entry={route} />
  ) : (
    <p>elsewhere</p>
  );
const showSessions = (
  sessions: SessionSummary[],
  initial: Route = { name: "sessions" },
) => renderRouted(testApp({ deployments: [deploymentWith(sessions)] }), initial, page);
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
    renderRouted(app, { name: "sessions" }, page);
    expect(screen.queryByTestId("session-child")).toBeNull();
    fireEvent.click(toggle("parent"));
    expect(depthOf("session-child")).toBe("1");
    const advanced = [
      child,
      session({
        latestRequestDocId: "physical-parent-2",
        latestRequestId: "logical-parent-2",
      }),
    ];
    act(() => publish(app, [deploymentWith(advanced)]));
    expect(depthOf("session-child")).toBe("1");
    expect(toggle("parent")).toHaveAttribute("aria-expanded", "true");
  });

  it("opens one level at a time, each session one step under its own starter, and closes a group with everything opened in it", () => {
    const grandchild = session({
      sessionId: "grandchild",
      title: "Helper",
      latestRequestDocId: "physical-grandchild",
      latestRequestId: "logical-grandchild",
      startedBy: {
        sessionId: "child",
        nodeDid: "did:test:owner",
        requesterDid: null,
        causeRequestDocId: "physical-child",
      },
    });
    showSessions([grandchild, child, parent]);
    fireEvent.click(toggle("parent"));
    expect(depthOf("session-child")).toBe("1");
    expect(screen.queryByTestId("session-grandchild")).toBeNull();
    fireEvent.click(toggle("child"));
    expect(depthOf("session-grandchild")).toBe("2");
    const order = screen
      .getAllByTestId(/^session-(parent|child|grandchild)$/)
      .map((row) => row.dataset.testid);
    expect(order).toEqual(["session-parent", "session-child", "session-grandchild"]);
    fireEvent.click(toggle("parent"));
    fireEvent.click(toggle("parent"));
    expect(toggle("child")).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByTestId("session-grandchild")).toBeNull();
  });

  it("does not nest under an identical session label in a different requester scope", () => {
    showSessions([child, session({ requesterDid: "did:test:other" })]);
    expect(depthOf("session-child")).toBe("0");
  });

  it("starts a visit closed and finds rows as they were left on the way back", () => {
    const finished = { ...child, turnState: "completed" };
    const { nav } = showSessions([finished, parent]);
    expect(toggle("parent")).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(toggle("parent"));
    expect(screen.getByTestId("session-child")).toBeInTheDocument();
    act(() => nav().navigate({ name: "session", sessionId: "child" }));
    act(() => nav().back());
    expect(screen.getByTestId("session-child")).toBeInTheDocument();
    act(() => nav().navigate({ name: "sessions" }));
    expect(screen.queryByTestId("session-child")).toBeNull();
  });

  it("goes into what a session started, with a trail back up", () => {
    showSessions([child, parent], {
      name: "sessionsUnder",
      session: sessionKeyOf(parent),
    });
    const trail = within(screen.getByTestId("sessions-trail"));
    expect(trail.getByRole("link", { name: "Sessions" })).toHaveAttribute(
      "href",
      "/sessions",
    );
    expect(trail.getByRole("heading", { name: "Lead" })).toBeInTheDocument();
    expect(depthOf("session-child")).toBe("0");
    expect(screen.queryByTestId("session-parent")).toBeNull();
  });

  it("links a session's count to what it started", () => {
    showSessions([child, parent]);
    expect(screen.getByTestId("sessions-under-parent")).toHaveAttribute(
      "href",
      `/sessions/under/${encodeURIComponent(sessionKeyOf(parent))}`,
    );
  });

  it("opens a big group to its first few and links to all of it", () => {
    const many = Array.from({ length: OPENED_SHOWN + 2 }, (_, n) =>
      session({
        ...child,
        sessionId: `w${n}`,
        title: `w${n}`,
        turnState: "completed",
      }),
    );
    showSessions([parent, ...many]);
    fireEvent.click(toggle("parent"));
    expect(screen.getAllByTestId(/^session-w\d+$/)).toHaveLength(OPENED_SHOWN);
    expect(screen.getByTestId("sessions-all-parent")).toHaveTextContent(
      `View all ${many.length}`,
    );
  });

  it("says how far a filter narrows the list, lists matches with where they sit, and clears", () => {
    const failed = { ...child, turnState: "failed" };
    act(() => listViews.setSessionFilter({ ...emptyFilter, states: ["failed"] }));
    showSessions([failed, parent]);
    expect(screen.getByTestId("sessions-narrowed")).toHaveTextContent(
      "1 of 2 sessions",
    );
    expect(screen.queryByTestId("session-parent")).toBeNull();
    expect(screen.getByTestId("session-child").closest("li")).toHaveTextContent(
      "in Lead",
    );
    fireEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(screen.queryByTestId("sessions-narrowed")).toBeNull();
    expect(screen.getByTestId("session-parent")).toBeInTheDocument();
    expect(screen.queryByTestId("session-child")).toBeNull();
  });

  it("nests a session under its nearest listed starter through one on a node the list leaves out", () => {
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
    renderRouted(
      testApp({
        deployments: [
          node({ nodeDid: "did:key:a", sessions: [lead, helper] }),
          node({ nodeDid: "did:key:b", sessions: [worker] }),
        ],
      }),
      { name: "sessions" },
      page,
    );
    expect(screen.queryByTestId("session-helper")).toBeNull();
    fireEvent.click(toggle("lead"));
    expect(depthOf("session-helper")).toBe("1");
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
