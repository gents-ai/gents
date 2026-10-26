import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryNavProvider } from "@gents/shell";
import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
  SessionSummary,
} from "@source-inc/gents-desktop-client";

import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { SessionScreen } from "../src/ui/screens/SessionScreen";
import { resetListViews } from "../src/ui/app/listViews";
import { resetPreferences } from "../src/ui/preferences";
import { sessionMarkdown } from "../src/ui/lib/session-markdown";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { href, type Route } from "../src/ui/lib/router";
import { node, renderRouted, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));
vi.mock("../src/ui/screens/BehaviorPicker", () => ({ AgentPicker: () => null }));
const toast = vi.hoisted(() => Object.assign(vi.fn(), { error: vi.fn() }));
vi.mock("sonner", () => ({ toast }));
vi.stubGlobal(
  "IntersectionObserver",
  class {
    observe() {}
    disconnect() {}
  },
);

const NODE = deployment.nodeDid;

const summary = (overrides: Partial<SessionSummary>): SessionSummary => ({
  sessionId: "first",
  nodeDid: NODE,
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: null,
  latestRequestId: null,
  closedAt: null,
  archivedAt: null,
  archiveRevision: "7",
  tags: [],
  provenance: null,
  title: "First",
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
const first = summary({});
const second = summary({ sessionId: "second", title: "Second" });

const reconstruction = { kind: "exact" } as never;
const user = (content: string, sequence: number): RenderedTimelineItem => ({
  kind: "userMessage",
  itemKey: `u${sequence}`,
  ownsTurn: true,
  sequence,
  content,
  timestamp: null,
  reconstruction,
});
const agent = (content: string, sequence: number): RenderedTimelineItem => ({
  kind: "assistantMessage",
  itemKey: `a${sequence}`,
  sequence,
  content,
  reasoning: null,
  timestamp: null,
  reconstruction,
});
const command = (sequence: number): RenderedTimelineItem => ({
  kind: "toolGroup",
  itemKey: `t${sequence}`,
  messageSequence: sequence,
  tools: [
    {
      itemKey: `call${sequence}`,
      toolName: "shell",
      statusKind: "success",
      reconstruction,
      presentation: {
        kind: "command",
        command: "ls -la",
        exitCode: 0,
        timedOut: false,
        failed: false,
        durationMs: 10,
        cwd: null,
        executionMode: null,
        networkMode: null,
        stdout: "",
        stderr: "",
        fallbackOutput: null,
      },
    } as never,
  ],
});

const page = (route: Route) =>
  route.name === "sessions" ? <SessionsScreen entry={route} /> : <p>elsewhere</p>;

function listApp(sessions: SessionSummary[], api: Record<string, unknown> = {}) {
  return testApp({
    api: {
      fetchDesktopSnapshot: vi.fn().mockResolvedValue({ bootstrap: {}, client: null }),
      ...api,
    },
    deployments: [node({ sessions })],
  });
}

function showList(sessions: SessionSummary[], api: Record<string, unknown> = {}) {
  const app = listApp(sessions, api);
  renderRouted(app, { name: "sessions" }, page);
  return app;
}

const rowTitles = () =>
  within(screen.getByRole("list", { name: "Sessions" }))
    .getAllByTestId(/^session-/)
    .map((row) => row.textContent);

const menuClosed = () => waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

/* the Copy items sit in a submenu beside the menu */
async function openCopy(u: ReturnType<typeof userEvent.setup>, menu: HTMLElement) {
  /* a pointer both hovers the submenu open and clicks it, which can close it again */
  within(menu).getByRole("menuitem", { name: "Copy" }).focus();
  await u.keyboard("{ArrowRight}");
  await waitFor(() => expect(screen.getAllByRole("menu").length).toBeGreaterThan(1));
  return screen.getAllByRole("menu").at(-1)!;
}

async function openRowMenu(title: string) {
  await menuClosed();
  /* an open submenu marks what is behind it pointer-events: none */
  const u = userEvent.setup({ pointerEventsCheck: 0 });
  await u.click(screen.getByRole("button", { name: `${title} actions` }));
  return { u, menu: await screen.findByRole("menu") };
}

beforeEach(() => {
  toast.mockClear();
  resetListViews();
  resetPreferences();
});
afterEach(() => {
  cleanup();
  resetPreferences();
});

describe("a session's menu in the sessions list", () => {
  it("lists the same items from a right-click as from its actions button", async () => {
    showList([first]);
    const u = userEvent.setup();
    await u.pointer({
      keys: "[MouseRight]",
      target: screen.getByTestId("session-first"),
    });
    const menu = await screen.findByRole("menu");
    const items = within(menu)
      .getAllByRole("menuitem")
      .map((item) => item.textContent);
    expect(items).toEqual(["Open", "Rename", "Pin", "Copy", "Archive"]);
    const copy = await openCopy(u, menu);
    expect(
      within(copy)
        .getAllByRole("menuitem")
        .map((item) => item.textContent),
    ).toEqual(["Session ID", "As Markdown"]);
  });

  it("pins a session to the head of the list with a marker, and unpins it", async () => {
    showList([first, second]);
    expect(rowTitles()).toEqual(["First", "Second"]);

    const { u, menu } = await openRowMenu("Second");
    await u.click(within(menu).getByRole("menuitem", { name: "Pin" }));
    expect(rowTitles()).toEqual(["Second", "First"]);
    const pinnedRow = screen.getByTestId("session-second").closest("li")!;
    expect(within(pinnedRow).getByRole("img", { name: "Pinned" })).toBeInTheDocument();

    const again = await openRowMenu("Second");
    await again.u.click(within(again.menu).getByRole("menuitem", { name: "Unpin" }));
    expect(rowTitles()).toEqual(["First", "Second"]);
    expect(screen.queryByRole("img", { name: "Pinned" })).toBeNull();
  });

  it("says when it pins or unpins, and Undo puts a pin back where it was", async () => {
    showList([first, second]);
    const undo = () => act(() => toast.mock.lastCall![1].action.onClick());
    for (const title of ["First", "Second"]) {
      const { u, menu } = await openRowMenu(title);
      await u.click(within(menu).getByRole("menuitem", { name: "Pin" }));
      expect(toast).toHaveBeenLastCalledWith("Pinned", expect.anything());
    }
    expect(rowTitles()).toEqual(["First", "Second"]);

    const { u, menu } = await openRowMenu("First");
    await u.click(within(menu).getByRole("menuitem", { name: "Unpin" }));
    expect(toast).toHaveBeenLastCalledWith("Unpinned", {
      action: expect.objectContaining({ label: "Undo" }),
    });
    expect(rowTitles()).toEqual(["Second", "First"]);
    undo();
    expect(rowTitles()).toEqual(["First", "Second"]);
    expect(screen.getAllByRole("img", { name: "Pinned" })).toHaveLength(2);
  });

  it("renames in the row, asking the session's own node", async () => {
    const renameSession = vi.fn().mockResolvedValue(undefined);
    const app = listApp([first], { renameSession });
    app.stores.selection.setState({ nodeDid: "did:key:other" });
    renderRouted(app, { name: "sessions" }, page);

    const { u, menu } = await openRowMenu("First");
    await u.click(within(menu).getByRole("menuitem", { name: "Rename" }));
    const field = screen.getByRole("textbox", { name: "Rename First" });
    await waitFor(() => expect(field).toHaveFocus());
    await u.clear(field);
    await u.type(field, "Renamed{Enter}");
    await waitFor(() =>
      expect(renameSession).toHaveBeenCalledWith({
        nodeDid: NODE,
        sessionId: "first",
        title: "Renamed",
      }),
    );
    expect(screen.queryByRole("textbox", { name: "Rename First" })).toBeNull();
  });

  it("keeps archived sessions on their own page, reached from the list's menu", async () => {
    const archivedOne = { ...second, title: "Old", archivedAt: "2026-10-26T00:00:00Z" };
    showList([first, archivedOne]);
    expect(rowTitles()).toEqual(["First"]);
    const u = userEvent.setup();
    await u.click(screen.getByRole("button", { name: "More session views" }));
    const view = await screen.findByRole("menuitem", { name: "View archived" });
    expect(view).toHaveAttribute("href", href({ name: "sessionsArchived" }));
    cleanup();

    renderRouted(listApp([first, archivedOne]), { name: "sessionsArchived" }, () => (
      <SessionsScreen archived entry={{}} />
    ));
    expect(rowTitles()).toEqual(["Old"]);
    expect(screen.getByRole("heading", { name: /Archived/ })).toBeInTheDocument();
  });

  it("says so when nothing is archived", () => {
    const app = listApp([first]);
    renderRouted(app, { name: "sessionsArchived" }, () => (
      <SessionsScreen archived entry={{}} />
    ));
    expect(screen.getByText("No archived sessions.")).toBeInTheDocument();
  });

  it("shows the new title once the app's copy of the session has it", async () => {
    const snapshot = (title: string) => ({
      bootstrap: {},
      client: { deployments: [node({ sessions: [{ ...first, title }] })] },
    });
    const fetchDesktopSnapshot = vi
      .fn()
      .mockResolvedValueOnce(snapshot("First"))
      .mockResolvedValue(snapshot("Renamed"));
    const app = showList([first], {
      renameSession: vi.fn().mockResolvedValue(undefined),
      fetchDesktopSnapshot,
    });
    await act(() => app.actions.renameSession("first", "Renamed", NODE));
    expect(fetchDesktopSnapshot).toHaveBeenCalledTimes(2);
    expect(rowTitles()).toContain("Renamed");
  });

  it("leaves the title as it was on Escape", async () => {
    const renameSession = vi.fn();
    showList([first], { renameSession });
    const { u, menu } = await openRowMenu("First");
    await u.click(within(menu).getByRole("menuitem", { name: "Rename" }));
    await u.type(screen.getByRole("textbox", { name: "Rename First" }), "x{Escape}");
    expect(screen.getByTestId("session-first")).toHaveTextContent("First");
    expect(renameSession).not.toHaveBeenCalled();
  });

  it("copies the session ID, and the working directory once one was picked", async () => {
    const app = listApp([first, second]);
    app.stores.chat.setState({ folders: { first: "/work/repo" } });
    renderRouted(app, { name: "sessions" }, page);

    let { u, menu } = await openRowMenu("First");
    await u.click(
      within(await openCopy(u, menu)).getByRole("menuitem", {
        name: "Working directory",
      }),
    );
    await waitFor(async () =>
      expect(await navigator.clipboard.readText()).toBe("/work/repo"),
    );

    ({ u, menu } = await openRowMenu("First"));
    await u.click(
      within(await openCopy(u, menu)).getByRole("menuitem", { name: "Session ID" }),
    );
    await waitFor(async () =>
      expect(await navigator.clipboard.readText()).toBe("first"),
    );

    ({ u, menu } = await openRowMenu("Second"));
    expect(
      within(await openCopy(u, menu)).queryByRole("menuitem", {
        name: "Working directory",
      }),
    ).toBeNull();
    await u.keyboard("{Escape}{Escape}");
  });

  it("copies the whole transcript as Markdown, reading every page", async () => {
    const fetchSessionRows = vi
      .fn()
      .mockResolvedValueOnce({
        sessionId: "first",
        nodeDid: NODE,
        rows: [agent("Done.", 4)],
        coveredFrom: 3,
        coveredThrough: null,
        hasOlder: true,
        hasNewer: false,
        complete: true,
        unreadable: null,
        queryCount: 1,
      })
      .mockResolvedValueOnce({
        sessionId: "first",
        nodeDid: NODE,
        rows: [user("List the files", 1), agent("Looking.", 2), command(2)],
        coveredFrom: 1,
        coveredThrough: 2,
        hasOlder: false,
        hasNewer: true,
        complete: true,
        unreadable: null,
        queryCount: 1,
      });
    showList([first], { fetchSessionRows });
    const { u, menu } = await openRowMenu("First");
    await u.click(
      within(await openCopy(u, menu)).getByRole("menuitem", { name: "As Markdown" }),
    );
    await waitFor(async () =>
      expect(await navigator.clipboard.readText()).toBe(
        "# First\n\n**You**\n\nList the files\n\n**Agent**\n\nLooking.\n\n- $ `ls -la` · exit 0\n\n**Agent**\n\nDone.\n",
      ),
    );
    expect(fetchSessionRows).toHaveBeenNthCalledWith(1, {
      sessionId: "first",
      nodeDid: NODE,
      window: { kind: "tip" },
      limit: 80,
    });
    expect(fetchSessionRows).toHaveBeenNthCalledWith(2, {
      sessionId: "first",
      nodeDid: NODE,
      window: { kind: "before", sequence: 3 },
      limit: 80,
    });
  });

  it("archives at the revision it shows, and leaves archived sessions out", async () => {
    const setSessionArchived = vi.fn().mockResolvedValue({
      nodeDid: NODE,
      sessionId: "first",
      archivedAt: "2026-10-26T00:00:00Z",
      revision: "8",
      outcome: "applied",
    });
    showList([first, summary({ sessionId: "gone", title: "Gone", archivedAt: "x" })], {
      setSessionArchived,
    });
    expect(rowTitles()).toEqual(["First"]);
    const { u, menu } = await openRowMenu("First");
    await u.click(within(menu).getByRole("menuitem", { name: "Archive" }));
    expect(setSessionArchived).toHaveBeenCalledWith({
      nodeDid: NODE,
      sessionId: "first",
      expectedRevision: "7",
      archived: true,
    });
  });

  it("says so when the session changed before the archive landed", async () => {
    const reportFailure = vi.fn();
    const app = testApp({
      api: {
        fetchDesktopSnapshot: vi
          .fn()
          .mockResolvedValue({ bootstrap: {}, client: null }),
        setSessionArchived: vi.fn().mockResolvedValue({
          nodeDid: NODE,
          sessionId: "first",
          archivedAt: null,
          revision: "9",
          outcome: "conflict",
        }),
      },
      deployments: [node({ sessions: [first] })],
      reportFailure,
    });
    await expect(app.actions.setSessionArchived(first, true)).rejects.toBeDefined();
    expect(reportFailure).toHaveBeenCalledWith(
      expect.stringContaining("archive the session"),
    );
  });
});

function heldSession(over: Partial<DesktopSessionSnapshot> = {}) {
  return {
    sessionId: "first",
    nodeDid: NODE,
    agentId: "default",
    title: "First",
    turnState: "completed",
    timelineItems: [],
    timelinePage: { hasOlder: false },
    latestRequestId: null,
    latestRequestOutcome: null,
    goal: null,
    context: null,
    queuedTurns: [],
    foldedInputs: [],
    ...over,
  } as unknown as DesktopSessionSnapshot;
}

function renderHeader(listed: SessionSummary, api: Record<string, unknown> = {}) {
  const app: DesktopApp = testApp({
    api: {
      sessionProvenance: vi.fn().mockResolvedValue(null),
      fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
      fetchSessionSnapshot: vi.fn().mockResolvedValue(null),
      fetchDesktopSnapshot: vi.fn().mockResolvedValue({ bootstrap: {}, client: null }),
      ...api,
    },
    deployments: [node({ sessions: [listed] })],
    session: heldSession(),
    selection: { nodeDid: NODE, sessionId: "first" },
  });
  render(
    <MemoryNavProvider initial={{ name: "session", sessionId: "first" }}>
      <SessionScreen />
    </MemoryNavProvider>,
    { wrapper: withApp(app) },
  );
  return app;
}

async function openHeaderMenu() {
  await menuClosed();
  const u = userEvent.setup();
  await u.click(screen.getAllByRole("button", { name: "More" })[0]!);
  return { u, menu: await screen.findByRole("menu") };
}

describe("a session's menu in its header", () => {
  it("leads the pane's menu with the session's items", async () => {
    renderHeader(first);
    const { menu } = await openHeaderMenu();
    const items = within(menu)
      .getAllByRole("menuitem")
      .map((item) => item.textContent);
    expect(items).toEqual(["Rename", "Pin", "Copy", "Archive"]);
  });

  it("starts the header's own rename", async () => {
    const renameSession = vi.fn().mockResolvedValue(undefined);
    renderHeader(first, { renameSession });
    const { u, menu } = await openHeaderMenu();
    await u.click(within(menu).getByRole("menuitem", { name: "Rename" }));
    const field = screen.getByRole("textbox", { name: "Rename First" });
    await waitFor(() => expect(field).toHaveFocus());
    await u.clear(field);
    await u.type(field, "Renamed{Enter}");
    await waitFor(() =>
      expect(renameSession).toHaveBeenCalledWith({
        nodeDid: NODE,
        sessionId: "first",
        title: "Renamed",
      }),
    );
  });

  it("unarchives an archived session", async () => {
    const setSessionArchived = vi.fn().mockResolvedValue({
      nodeDid: NODE,
      sessionId: "first",
      archivedAt: null,
      revision: "8",
      outcome: "applied",
    });
    renderHeader(summary({ archivedAt: "2026-10-26T00:00:00Z" }), {
      setSessionArchived,
    });
    const { u, menu } = await openHeaderMenu();
    expect(within(menu).queryByRole("menuitem", { name: "Archive" })).toBeNull();
    await u.click(within(menu).getByRole("menuitem", { name: "Unarchive" }));
    expect(setSessionArchived).toHaveBeenCalledWith({
      nodeDid: NODE,
      sessionId: "first",
      expectedRevision: "7",
      archived: false,
    });
  });

  it("pins from the header", async () => {
    renderHeader(first);
    const { u, menu } = await openHeaderMenu();
    await u.click(within(menu).getByRole("menuitem", { name: "Pin" }));
    expect(toast).toHaveBeenLastCalledWith("Pinned", expect.anything());
    const again = await openHeaderMenu();
    expect(
      within(again.menu).getByRole("menuitem", { name: "Unpin" }),
    ).toBeInTheDocument();
    await u.keyboard("{Escape}");
    act(() => toast.mock.lastCall![1].action.onClick());
    const after = await openHeaderMenu();
    expect(
      within(after.menu).getByRole("menuitem", { name: "Pin" }),
    ).toBeInTheDocument();
  });
});

describe("a transcript as Markdown", () => {
  it("skips what nobody said and keeps tool calls to a line each", () => {
    expect(
      sessionMarkdown("Plan", [
        user("  Hi  ", 1),
        agent("", 2),
        command(2),
        command(3),
        {
          kind: "sessionCompaction",
          itemKey: "c",
          sequence: 4,
          messagesCompacted: 3,
          originalTokens: null,
          compactedTokens: null,
          timestamp: null,
        },
      ]),
    ).toBe("# Plan\n\n**You**\n\nHi\n\n- $ `ls -la` · exit 0\n- $ `ls -la` · exit 0\n");
  });
});
