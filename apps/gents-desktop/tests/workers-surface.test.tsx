import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { DockTabs } from "@/app/Dock";
import { getSurface } from "@/app/surfaces";
import { workspace } from "@/app/workspace";
import { startedUnder, startedUnderId } from "@/hooks/useFleet";
import { StartedSessionsButton } from "../src/ui/screens/StartedSessionsButton";
import { WorkersSurface } from "../src/ui/screens/WorkersSurface";
import "../src/ui/screens/surfaces";
import { node, testApp, withApp } from "./app-fixture";

const session = (overrides: Partial<SessionSummary>): SessionSummary => ({
  sessionId: "parent",
  nodeDid: "did:test:owner",
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: "physical-parent",
  latestRequestId: "logical-parent",
  closedAt: null,
  archivedAt: null,
  archiveRevision: "",
  tags: [],
  provenance: null,
  title: "Lead",
  previewText: null,
  status: null,
  agentId: null,
  taskId: null,
  taskName: null,
  triggerId: null,
  triggerKind: null,
  createdAt: null,
  updatedAt: null,
  turnState: "completed",
  messageCount: null,
  toolCallCount: null,
  ...overrides,
});

const startedBy = (sessionId: string) => ({
  sessionId,
  nodeDid: "did:test:owner",
  requesterDid: null,
  causeRequestDocId: `physical-${sessionId}`,
});

const worker = (sessionId: string, overrides: Partial<SessionSummary>) =>
  session({
    sessionId,
    title: sessionId,
    latestRequestDocId: `physical-${sessionId}`,
    latestRequestId: `logical-${sessionId}`,
    startedBy: startedBy("parent"),
    ...overrides,
  });

describe("the started sessions dock", () => {
  beforeEach(() => workspace.reset());

  it("lists started sessions newest first, and shows what one started in place", () => {
    const sessions = [
      session({}),
      worker("older", {
        createdAt: "2026-10-20T10:00:00Z",
        updatedAt: "2026-10-20T10:02:00Z",
      }),
      worker("newer", { createdAt: "2026-10-21T10:00:00Z", turnState: "failed" }),
      worker("busy", { createdAt: "2026-10-19T10:00:00Z", turnState: "running" }),
      worker("grandchild", { turnState: "running", startedBy: startedBy("older") }),
    ];
    render(<WorkersSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(
        testApp({ deployments: [node({ nodeDid: "did:key:node", sessions })] }),
      ),
    });
    const rows = screen.getAllByRole("listitem");
    expect(rows.map((row) => within(row).getAllByRole("link")[0]!.textContent)).toEqual(
      ["newer", "older", "busy"],
    );
    expect(within(rows[0]!).getByRole("img", { name: "Failed" })).toBeInTheDocument();
    expect(rows[1]).toHaveTextContent("2m");
    expect(rows[2]).toHaveTextContent(/\d+[mhd]/);
    expect(screen.queryByRole("link", { name: "grandchild" })).toBeNull();
    fireEvent.click(
      within(rows[1]!).getByRole("button", {
        name: "Show 1 started session · 1 running",
      }),
    );
    const nested = within(rows[1]!).getAllByRole("listitem");
    expect(
      nested.map((row) => within(row).getAllByRole("link")[0]!.textContent),
    ).toEqual(["grandchild"]);
    expect(
      within(rows[1]!).getByRole("button", {
        name: "Hide 1 started session · 1 running",
      }),
    ).toHaveAttribute("aria-expanded", "true");
  });
});

const under = (parent: string, sessionId: string, turnState: string) =>
  worker(sessionId, { turnState, startedBy: startedBy(parent) });

const appWith = (sessions: SessionSummary[]) =>
  testApp({ deployments: [node({ nodeDid: "did:key:node", sessions })] });

/* the lead's workers: a failure two levels down, a stop on a worker and
   another three levels down, one running, and a turn replaced by an edit */
const fleet = () => [
  session({}),
  worker("quiet", { createdAt: "2026-10-25T10:00:00Z" }),
  under("quiet", "deep", "failed"),
  worker("halted", { createdAt: "2026-10-24T10:00:00Z", turnState: "interrupted" }),
  worker("busy", { createdAt: "2026-10-23T10:00:00Z", turnState: "running" }),
  worker("replaced", { createdAt: "2026-10-22T10:00:00Z", turnState: "superseded" }),
  worker("middle", { createdAt: "2026-10-21T10:00:00Z" }),
  under("middle", "inner", "completed"),
  under("inner", "innermost", "interrupted"),
];

const LEAD = "5 started sessions · 1 running · 1 failed · 2 stopped";

describe("what the started sessions dock counts", () => {
  beforeEach(() => workspace.reset());

  it("lifts failed, stopped and running sessions from any depth, but not a replaced turn", () => {
    const state = appWith(fleet()).stores.fleet.getState();
    expect(startedUnderId(state, "parent")).toEqual({
      started: 5,
      running: 1,
      failed: 1,
      stopped: 2,
    });
    expect(startedUnderId(state, "middle")).toEqual({
      started: 1,
      running: 0,
      failed: 0,
      stopped: 1,
    });
    expect(startedUnderId(state, "busy")).toEqual({
      started: 0,
      running: 0,
      failed: 0,
      stopped: 0,
    });
  });

  it("ends a loop in who started whom where it began", () => {
    const state = appWith([
      under("b", "a", "failed"),
      under("a", "b", "completed"),
    ]).stores.fleet.getState();
    expect(startedUnder(state, state.bySessionId["a"]!)).toEqual({
      started: 1,
      running: 0,
      failed: 0,
      stopped: 0,
    });
  });

  it("names them on the tab, the header and each row alike, red while any failed", () => {
    const app = appWith(fleet());
    expect(
      getSurface("workers")!.badge!(app.stores.fleet.getState(), "parent"),
    ).toEqual({ count: 5, label: LEAD, mark: "alert" });
    workspace.openSurface("session:parent", "workers");
    render(
      <>
        <DockTabs sessionId="parent" routeName="session" />
        <StartedSessionsButton sessionId="parent" />
        <WorkersSurface placement="dock" sessionId="parent" />
      </>,
      { wrapper: withApp(app) },
    );
    expect(screen.getByRole("tab", { name: `Started sessions ${LEAD}` })).toBeVisible();
    expect(screen.getByTestId("tab-badge-mark")).toHaveClass("bg-destructive");
    expect(screen.getByRole("button", { name: LEAD })).toBeInTheDocument();

    const row = (name: string) =>
      screen.getByRole("link", { name }).closest("li") as HTMLElement;
    expect(
      within(row("quiet")).getByRole("button", {
        name: "Show 1 started session · 1 failed",
      }),
    ).toBeInTheDocument();
    expect(within(row("quiet")).getByTestId("started-mark")).toHaveClass(
      "bg-destructive",
    );
    for (const name of ["halted", "busy", "replaced"])
      expect(within(row(name)).queryByRole("button")).toBeNull();
    fireEvent.click(
      within(row("middle")).getByRole("button", {
        name: "Show 1 started session · 1 stopped",
      }),
    );
    expect(within(row("middle")).queryByTestId("started-mark")).toBeNull();
    expect(
      within(row("inner")).getByRole("button", {
        name: "Show 1 started session · 1 stopped",
      }),
    ).toBeInTheDocument();
  });

  it("marks the tab while a session under a worker runs", () => {
    const app = appWith([
      session({}),
      worker("done", {}),
      under("done", "going", "running"),
    ]);
    workspace.openSurface("session:parent", "workers");
    render(
      <>
        <DockTabs sessionId="parent" routeName="session" />
        <StartedSessionsButton sessionId="parent" />
      </>,
      { wrapper: withApp(app) },
    );
    const label = "1 started session · 1 running";
    expect(
      screen.getByRole("tab", { name: `Started sessions ${label}` }),
    ).toBeVisible();
    expect(screen.getByTestId("tab-badge-mark")).toHaveClass("bg-brand");
    expect(screen.getByRole("button", { name: label })).toBeInTheDocument();
  });
});
