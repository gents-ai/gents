import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { DockTabs } from "@/app/Dock";
import { getSurface } from "@/app/surfaces";
import { workspace } from "@/app/workspace";
import { startedUnder, startedUnderId } from "@/hooks/useFleet";
import { resetSessionViews, seenMarks } from "../src/ui/app/sessionViews";
import { sessionKeyOf } from "../src/hooks/fleetStore";
import { StartedSessionsButton } from "../src/ui/screens/StartedSessionsButton";
import { WorkersSurface } from "../src/ui/screens/WorkersSurface";
import "../src/ui/screens/surfaces";
import { node, publish, testApp, withApp } from "./app-fixture";

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
  agentDid: null,
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

  it("says how long each worked, to the second", () => {
    const sessions = [
      session({}),
      worker("quick", {
        createdAt: "2026-10-20T10:00:00Z",
        updatedAt: "2026-10-20T10:00:05Z",
        turnState: "completed",
      }),
      worker("slow", {
        createdAt: "2026-10-20T09:00:00Z",
        updatedAt: "2026-10-20T09:01:11Z",
        turnState: "completed",
      }),
    ];
    render(<WorkersSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(
        testApp({ deployments: [node({ nodeDid: "did:key:node", sessions })] }),
      ),
    });
    const [quick, slow] = screen.getAllByRole("listitem");
    expect(within(quick!).getByTestId("worker-took")).toHaveTextContent("5s");
    expect(within(slow!).getByTestId("worker-took")).toHaveTextContent("1m 11s");
  });

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
    expect(within(rows[1]!).getAllByTestId("worker-took")[0]).toHaveTextContent(/^2m$/);
    expect(within(rows[2]!).getAllByTestId("worker-took")[0]).toHaveTextContent(
      /^\d+[smhd]/,
    );
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
    expect(screen.getByRole("tab", { name: `Started ${LEAD}` })).toBeVisible();
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
    expect(screen.getByRole("tab", { name: `Started ${label}` })).toBeVisible();
    expect(screen.getByTestId("tab-badge-mark")).toHaveClass("bg-brand");
    expect(screen.getByRole("button", { name: label })).toBeInTheDocument();
  });
});

const later = () => new Date(Date.now() + 60_000).toISOString();

describe("what each started session shows", () => {
  beforeEach(() => {
    workspace.reset();
    resetSessionViews();
  });

  const row = (name: string) =>
    screen.getByRole("link", { name }).closest("li") as HTMLElement;

  it("shows a worker's last reply once it has stopped working", () => {
    const reply = "fast done, with the release notes drafted";
    render(<WorkersSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(
        appWith([
          session({}),
          worker("done", { replyPreview: reply }),
          worker("broke", { turnState: "failed", replyPreview: "could not reach it" }),
          worker("going", { turnState: "running", replyPreview: "half a thou" }),
          worker("quiet", { replyPreview: "  " }),
        ]),
      ),
    });
    expect(within(row("done")).getByText(reply)).toHaveClass("truncate");
    expect(within(row("done")).getByTestId("worker-reply")).toHaveTextContent(
      `⎿${reply}`,
    );
    /* the full reply is offered only where the line cuts it off */
    expect(screen.getByRole("link", { name: "done" })).not.toHaveAttribute("title");
    expect(within(row("broke")).getByText("could not reach it")).toBeInTheDocument();
    expect(within(row("going")).queryByText("half a thou")).toBeNull();
    expect(screen.getByRole("link", { name: "going" })).not.toHaveAttribute("title");
    expect(screen.getByRole("link", { name: "quiet" })).not.toHaveAttribute("title");
    expect(row("quiet").querySelectorAll("p")).toHaveLength(0);
  });

  it("offers a cut-off reply in full on hover", () => {
    const width = vi
      .spyOn(HTMLElement.prototype, "scrollWidth", "get")
      .mockReturnValue(500);
    const reply = "a reply long enough that one line cuts it off";
    render(<WorkersSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(appWith([session({}), worker("long", { replyPreview: reply })])),
    });
    expect(screen.getByRole("link", { name: "long" })).toHaveAttribute("title", reply);
    width.mockRestore();
  });

  it("marks a worker that finished since you last looked at it", () => {
    const sessions = [
      session({}),
      worker("unseen", { updatedAt: later() }),
      worker("failed", { turnState: "failed", updatedAt: later() }),
      worker("seen", { updatedAt: later() }),
      worker("going", { turnState: "running", updatedAt: later() }),
    ];
    const app = appWith(sessions);
    seenMarks.set(sessionKeyOf(sessions[3]!), { request: "logical-seen", done: true });
    render(<WorkersSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    expect(
      within(row("unseen")).getByRole("img", {
        name: "Finished · new since you looked",
      }),
    ).toBeInTheDocument();
    expect(
      within(row("failed")).getByRole("img", { name: "Failed · new since you looked" }),
    ).toBeInTheDocument();
    expect(
      within(row("seen")).getByRole("img", { name: "Finished" }),
    ).toBeInTheDocument();
    expect(
      within(row("going")).queryByRole("img", { name: /new since you looked/ }),
    ).toBeNull();
  });

  it("says nothing was handed out only once the session itself is listed", () => {
    const app = appWith([]);
    render(<WorkersSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    /* the panel keeps its empty state while the session itself is missing */
    expect(screen.getByText("No started sessions")).toBeInTheDocument();
    expect(screen.queryByRole("link")).toBeNull();
    act(() =>
      publish(app, [
        node({
          nodeDid: "did:key:node",
          sessions: [session({}), worker("helper", {})],
        }),
      ]),
    );
    expect(screen.queryByText("No started sessions")).toBeNull();
    expect(screen.getByRole("link", { name: "helper" })).toBeInTheDocument();
  });
});

describe("how a started session's changes ease in", () => {
  beforeEach(() => {
    workspace.reset();
    resetSessionViews();
  });

  it("eases in a reply and a state that change while the panel is open, not what it opened with", () => {
    const app = appWith([
      session({}),
      worker("early", { replyPreview: "already said" }),
      worker("late", { turnState: "running" }),
    ]);
    render(<WorkersSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(app),
    });
    const line = (text: string) =>
      screen.getByText(text).closest("[data-testid=worker-reply]")!.parentElement!;
    const mark = (name: string) =>
      within(screen.getByRole("link", { name }).closest("li") as HTMLElement).getByRole(
        "img",
      ).parentElement!;
    expect(line("already said")).not.toHaveAttribute("data-entering");
    expect(mark("late")).not.toHaveAttribute("data-entering");

    act(() =>
      publish(app, [
        node({
          nodeDid: "did:key:node",
          sessions: [
            session({}),
            worker("early", { replyPreview: "already said" }),
            worker("late", { replyPreview: "just finished" }),
          ],
        }),
      ]),
    );
    expect(line("just finished")).toHaveAttribute("data-entering");
    expect(line("already said")).not.toHaveAttribute("data-entering");
    expect(mark("late")).toHaveAttribute("data-entering");
    expect(mark("early")).not.toHaveAttribute("data-entering");
  });
});
