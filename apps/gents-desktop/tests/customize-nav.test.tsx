import { act, fireEvent, renderHook, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { MailboxItemView, SessionSummary } from "@source-inc/gents-desktop-client";
import { listViews, useListViews } from "../src/ui/app/listViews";
import { isListed, SECTIONS } from "../src/ui/screens/agent/sections";
import { mailboxSeen, resetMailboxSeen } from "../src/ui/app/mailboxSeen";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { afterEach, describe, expect, it } from "vitest";

import {
  CustomizeNavDialog,
  NavMenu,
  openCustomizeNav,
} from "../src/ui/app/CustomizeNav";
import {
  APP_FOOTER_ROWS,
  APP_NAV_ROWS,
  arrange,
  configRow,
  NAV_SECTIONS,
  drawNavRows,
  NAV_STATUS,
  useNavFooter,
} from "../src/ui/app/navLayout";
import { NavPanel } from "../src/ui/app/RailFlyout";
import {
  DEFAULT_NAV_LAYOUT,
  preferences,
  useNavLayout,
  useNavMode,
  restorePreferences,
  resetPreferences,
} from "../src/ui/preferences";
import { sessionKeyOf } from "../src/hooks/fleetStore";
import { deployment } from "./config-panel-wiring/fixtures";
import { node, renderIn, testApp, withFlags } from "./app-fixture";

afterEach(() => resetPreferences());

/* the app's own items and its configuration pages, as the nav offers them */
const ROWS = [
  ...APP_NAV_ROWS,
  ...SECTIONS.filter(isListed).map((section) => configRow(section, true)),
];

const ids = (layout: Parameters<typeof arrange>[1]) =>
  arrange(ROWS, layout)
    .filter(({ shown }) => shown)
    .map(({ row }) => row.id);

describe("arranging the nav", () => {
  it("gives every entry its own id, since an entry is shown or hidden by id", () => {
    const ids = [...ROWS, ...NAV_SECTIONS, ...APP_FOOTER_ROWS, NAV_STATUS].map(
      (entry) => entry.id,
    );
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("shows the app's own items in their order, and the optional ones only once chosen", () => {
    expect(ids({ order: [], shown: {} })).toEqual([
      "new-session",
      "mailbox",
      "sessions",
    ]);
    expect(ids({ order: [], shown: { "config:tasks": true, mailbox: false } })).toEqual(
      ["new-session", "sessions", "config:tasks"],
    );
  });

  it("follows the saved order, keeping an item it doesn't name after the named ones", () => {
    expect(ids({ order: ["sessions", "gone", "new-session"], shown: {} })).toEqual([
      "sessions",
      "new-session",
      "mailbox",
    ]);
  });

  it("restores only well-formed layouts", () => {
    const defaults = {
      ...restorePreferences({}, {} as never),
      navLayout: DEFAULT_NAV_LAYOUT,
    };
    expect(
      restorePreferences(
        { navLayout: { order: ["mailbox", 3], shown: { tasks: true, mailbox: "no" } } },
        defaults,
      ).navLayout,
    ).toEqual({ order: ["mailbox"], shown: { tasks: true } });
    expect(
      restorePreferences(
        { navLayout: { listed: { "mailbox-decide": 3, "mailbox-done": 7 } } },
        defaults,
      ).navLayout.listed,
    ).toEqual({ "mailbox-decide": 3 });
    expect(restorePreferences({ navLayout: "x" }, defaults).navLayout).toEqual(
      defaults.navLayout,
    );
  });
});

const session = (n: number): SessionSummary => ({
  sessionId: `s${n}`,
  nodeDid: deployment.nodeDid,
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: null,
  latestRequestId: null,
  closedAt: null,
  archivedAt: null,
  archiveRevision: "1",
  tags: [],
  provenance: null,
  title: `Session ${n}`,
  previewText: null,
  status: null,
  agentId: null,
  taskId: null,
  taskName: null,
  triggerId: null,
  triggerKind: null,
  createdAt: null,
  updatedAt: `2026-10-26T${String(10 + n).padStart(2, "0")}:00:00Z`,
  turnState: "completed",
  messageCount: null,
  toolCallCount: null,
});

function panel(sessions: SessionSummary[] = []) {
  return renderIn(
    testApp({ deployments: [node({ sessions })] }),
    <TooltipProvider>
      <NavPanel route={{ name: "sessions" }} settings={null} syncRow={false} />
      <CustomizeNavDialog />
    </TooltipProvider>,
  );
}

/* the nav's top items, in the order drawn */
const TOP = ["New session", "Mailbox", "Sessions", "Tasks"];
const labels = () =>
  screen
    .getAllByRole("link", { hidden: true })
    /* a row also holds its key hint and count, after its label */
    .map((link) => TOP.find((name) => link.textContent?.startsWith(name)))
    .filter((name) => name !== undefined);

describe("customizing the nav", () => {
  it("exposes the sidebar customization sheet as a modal dialog", () => {
    withFlags({ "customize-nav": true });
    panel();
    act(() => openCustomizeNav());
    expect(screen.getByRole("dialog", { name: "Customize navigation" })).toHaveAttribute(
      "aria-modal",
      "true",
    );
  });

  it("ignores a saved layout while the flag is off", () => {
    withFlags({ "customize-nav": false });
    act(() =>
      preferences.setNavLayout({
        order: ["sessions"],
        shown: { mailbox: false, "config:tasks": true },
      }),
    );
    panel();
    expect(labels()).toEqual(["New session", "Mailbox", "Sessions"]);
  });

  it("reorders, removes and adds items from the sheet, and restores the defaults", async () => {
    withFlags({ "customize-nav": true });
    panel();
    act(() => openCustomizeNav());
    const sheet = within(screen.getByTestId("customize-nav"));
    /* the sheet lists what the nav shows; the rest waits under Add */
    expect(sheet.queryByRole("button", { name: "Move Tasks" })).toBeNull();

    fireEvent.click(sheet.getByRole("button", { name: "Remove Mailbox" }));
    fireEvent.click(sheet.getByRole("button", { name: "Add to Items" }));
    /* the node's configuration pages are offered together, a level down */
    expect(screen.queryByRole("menuitem", { name: "Tasks" })).toBeNull();
    (await screen.findByRole("menuitem", { name: "Configuration" })).focus();
    await userEvent.setup({ pointerEventsCheck: 0 }).keyboard("{ArrowRight}");
    fireEvent.click(await screen.findByRole("menuitem", { name: "Tasks" }));
    fireEvent.keyDown(sheet.getByRole("button", { name: "Move Sessions" }), {
      key: "ArrowUp",
    });
    expect(labels()).toEqual(["Sessions", "New session", "Tasks"]);
    expect(sheet.queryByRole("button", { name: "Move Mailbox" })).toBeNull();

    fireEvent.click(sheet.getByRole("button", { name: "Restore defaults" }));
    expect(labels()).toEqual(["New session", "Mailbox", "Sessions"]);
    expect(sheet.getByRole("button", { name: "Restore defaults" })).toBeDisabled();
  });

  it("removes and adds back the foot's rows and the sync status, keeping Settings", async () => {
    withFlags({ "customize-nav": true });
    const footer = renderHook(() => useNavFooter());
    renderIn(
      testApp({ deployments: [node()] }),
      <TooltipProvider>
        <NavPanel
          route={{ name: "sessions" }}
          settings={<a href="/settings">Settings</a>}
          syncRow
        />
        <CustomizeNavDialog />
      </TooltipProvider>,
    );
    act(() => openCustomizeNav());
    const sheet = within(screen.getByTestId("customize-nav"));
    const nodes = () => screen.queryByRole("link", { name: /^Nodes/, hidden: true });

    fireEvent.click(sheet.getByRole("button", { name: "Remove Nodes" }));
    fireEvent.click(sheet.getByRole("button", { name: "Remove Sync status" }));
    expect(nodes()).toBeNull();
    expect(footer.result.current.status).toBe(false);
    expect(
      screen.getByRole("link", { name: "Settings", hidden: true }),
    ).toBeInTheDocument();

    fireEvent.click(sheet.getByRole("button", { name: "Add to Bottom" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Sync status" }));
    fireEvent.click(sheet.getByRole("button", { name: "Add to Bottom" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Nodes" }));
    expect(nodes()).not.toBeNull();
    expect(footer.result.current.status).toBe(true);
  });
});

const headings = () =>
  screen
    .queryAllByText(/^(Pinned|Recent)$/, { selector: "p" })
    .map((p) => p.textContent);
const sessionLinks = () =>
  screen
    .getAllByRole("link", { hidden: true })
    .filter((a) => /Session \d+$/.test(a.textContent ?? ""));

describe("the expanded nav's sections", () => {
  const sessions = Array.from({ length: 10 }, (_, i) => session(i));

  it("reorders and hides Pinned and Recent, and lists as many recent sessions as chosen", async () => {
    withFlags({ "customize-nav": true });
    preferences.setPinned(sessionKeyOf(sessions[0]!), true);
    panel(sessions);
    expect(headings()).toEqual(["Pinned", "Recent"]);
    /* one pinned, and the eight most recent */
    expect(sessionLinks()).toHaveLength(9);

    act(() => openCustomizeNav());
    const sheet = within(screen.getByTestId("customize-nav"));
    fireEvent.keyDown(sheet.getByRole("button", { name: "Move Recent" }), {
      key: "ArrowUp",
    });
    expect(headings()).toEqual(["Recent", "Pinned"]);

    fireEvent.click(sheet.getByRole("button", { name: "Recent sessions shown: 8" }));
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "5 sessions" }));
    expect(sessionLinks()).toHaveLength(6);

    fireEvent.click(sheet.getByRole("button", { name: "Remove Pinned" }));
    expect(headings()).toEqual(["Recent"]);
    fireEvent.click(sheet.getByRole("button", { name: /^Add to Sections/ }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Pinned" }));
    expect(headings()).toEqual(["Recent", "Pinned"]);
  });

  it("draws the default sections while the flag is off", () => {
    withFlags({ "customize-nav": false });
    act(() =>
      preferences.setNavLayout({
        order: ["recent"],
        shown: { pinned: false },
        recent: 5,
      }),
    );
    preferences.setPinned(sessionKeyOf(sessions[0]!), true);
    panel(sessions);
    expect(headings()).toEqual(["Pinned", "Recent"]);
    expect(sessionLinks()).toHaveLength(9);
  });
});

describe("the nav's context menu", () => {
  it("sets how the side nav shows from its submenu, and opens the sheet", async () => {
    withFlags({ "customize-nav": true });
    const mode = renderHook(() => useNavMode());
    renderIn(
      testApp({ deployments: [node()] }),
      <>
        <NavMenu render={<nav data-testid="nav" />}>nav</NavMenu>
        <CustomizeNavDialog />
      </>,
    );
    fireEvent.contextMenu(screen.getByTestId("nav"));
    /* the modes sit a level down; the keyboard opens it once, where a
       pointer both hovers it open and clicks it, which can close it again */
    screen.getByRole("menuitem", { name: "Side nav" }).focus();
    await userEvent.setup({ pointerEventsCheck: 0 }).keyboard("{ArrowRight}");
    expect(
      await screen.findByRole("menuitemradio", { name: "Open on hover" }),
    ).toHaveAttribute("aria-checked", "true");
    fireEvent.click(screen.getByRole("menuitemradio", { name: "Always open" }));
    expect(mode.result.current).toBe("expanded");

    fireEvent.contextMenu(screen.getByTestId("nav"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Customize…" }));
    expect(screen.getByTestId("customize-nav")).toBeInTheDocument();
  });

  it("is not there while the flag is off", () => {
    withFlags({ "customize-nav": false });
    renderIn(
      testApp({ deployments: [node()] }),
      <NavMenu render={<nav data-testid="nav" />}>nav</NavMenu>,
    );
    fireEvent.contextMenu(screen.getByTestId("nav"));
    expect(screen.queryByRole("menu")).toBeNull();
  });
});

const mailboxItem = (n: number): MailboxItemView => ({
  itemId: `m${n}`,
  itemKey: `m${n}`,
  requesterDid: "did:key:person",
  nodeDid: deployment.nodeDid,
  status: "open",
  kind: "ask",
  action: "reply",
  title: `Question ${n}`,
  summary: null,
  payload: null,
  sourceKind: "session",
  sourceId: `s${n}`,
  sessionId: null,
  requestId: null,
  causeDocId: null,
  targetNodeDid: deployment.nodeDid,
  targetAgentId: "default",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: `2026-10-06T0${n}:00:00.000Z`,
});

describe("the expanded nav's mailbox sections", () => {
  const show = (mailboxItems: MailboxItemView[]) =>
    renderIn(
      testApp({ deployments: [node({ mailboxItems })] }),
      <TooltipProvider>
        <NavPanel route={{ name: "sessions" }} settings={null} syncRow={false} />
        <CustomizeNavDialog />
      </TooltipProvider>,
    );
  const rows = () =>
    within(screen.getByRole("list", { name: "Needs a decision", hidden: true }))
      .getAllByRole("button", { hidden: true })
      .map((b) => b.textContent);

  it("adds a category from the sheet, under its name, listing its first five items and opening one", async () => {
    withFlags({ "customize-nav": true });
    show(Array.from({ length: 7 }, (_, i) => mailboxItem(i + 1)));
    expect(screen.queryByRole("list", { name: "Needs a decision" })).toBeNull();

    act(() => openCustomizeNav());
    const sheet = within(screen.getByTestId("customize-nav"));
    fireEvent.click(sheet.getByRole("button", { name: /^Add to Sections/ }));
    /* the mailbox's categories are offered together, a level down */
    expect(screen.queryByRole("menuitem", { name: "Needs a decision" })).toBeNull();
    (await screen.findByRole("menuitem", { name: "Mailbox" })).focus();
    await userEvent.setup({ pointerEventsCheck: 0 }).keyboard("{ArrowRight}");
    fireEvent.click(await screen.findByRole("menuitem", { name: "Needs a decision" }));
    /* listed in the sheet with what it belongs to */
    expect(sheet.getByText("· Mailbox", { exact: false })).toBeInTheDocument();
    act(() => listViews.setMailboxOpen(null));

    expect(rows()).toHaveLength(5);
    expect(screen.getByText("2 more in Mailbox")).toBeInTheDocument();
    /* the heading says how many wait, of which the first few are listed */
    expect(screen.getByText(/^Needs a decision/, { selector: "p" })).toHaveTextContent(
      "Needs a decision · 7",
    );
    fireEvent.click(
      sheet.getByRole("button", { name: "Needs a decision items shown: 5" }),
    );
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "3 items" }));
    expect(rows()).toHaveLength(3);
    expect(screen.getByText("4 more in Mailbox")).toBeInTheDocument();
    const open = renderHook(() => useListViews((views) => views.mailboxOpen));
    const first = within(
      screen.getByRole("list", { name: "Needs a decision", hidden: true }),
    ).getAllByRole("button", { hidden: true })[0]!;
    fireEvent.click(first);
    expect(open.result.current?.itemId).toBe(
      `m${first.textContent!.replace("Question ", "")}`,
    );
  });

  it("marks an item new since the person looked the way a session is marked", () => {
    withFlags({ "customize-nav": true });
    act(() => {
      resetMailboxSeen();
      mailboxSeen.start("2026-10-06T02:30:00.000Z");
      preferences.setNavLayout({ order: [], shown: { "mailbox-decide": true } });
    });
    show([mailboxItem(1), mailboxItem(3)]);
    expect(
      screen.getAllByRole("img", { name: /new since you looked/, hidden: true }),
    ).toHaveLength(1);
    expect(screen.getAllByRole("img", { name: "Question", hidden: true })).toHaveLength(
      1,
    );
    act(() => resetMailboxSeen());
  });

  it("lists every open item under All, what needs a decision first", () => {
    withFlags({ "customize-nav": true });
    act(() => preferences.setNavLayout({ order: [], shown: { "mailbox-all": true } }));
    show([
      {
        ...mailboxItem(1),
        kind: "finished",
        title: "Finished 1",
        createdAt: "2026-10-06T05:00:00.000Z",
      },
      {
        ...mailboxItem(2),
        kind: "failed",
        title: "Failed 2",
        createdAt: "2026-10-06T04:00:00.000Z",
      },
      { ...mailboxItem(3), createdAt: "2026-10-06T01:00:00.000Z" },
    ]);
    expect(
      within(screen.getByRole("list", { name: "Mailbox", hidden: true }))
        .getAllByRole("button", { hidden: true })
        .map((b) => b.textContent),
    ).toEqual(["Question 3", "Failed 2", "Finished 1"]);
  });

  it("offers All above the categories it covers", async () => {
    withFlags({ "customize-nav": true });
    show([]);
    act(() => openCustomizeNav());
    const sheet = within(screen.getByTestId("customize-nav"));
    fireEvent.click(
      sheet.getByRole("button", { name: "Add to Sections · in the expanded nav" }),
    );
    (await screen.findByRole("menuitem", { name: "Mailbox" })).focus();
    await userEvent.setup({ pointerEventsCheck: 0 }).keyboard("{ArrowRight}");
    const all = await screen.findByRole("menuitem", { name: "All" });
    const menu = all.closest("[role=menu]")!;
    expect(
      within(menu as HTMLElement)
        .getAllByRole("menuitem")
        .map((i) => i.textContent),
    ).toEqual(["All", "Needs a decision", "Went wrong", "Worth a look", "Done"]);
    expect(within(menu as HTMLElement).getAllByRole("separator")).toHaveLength(1);
  });

  it("isn't drawn while its category is empty", () => {
    withFlags({ "customize-nav": true });
    act(() =>
      preferences.setNavLayout({ order: [], shown: { "mailbox-decide": true } }),
    );
    show([]);
    expect(screen.queryByText("Needs a decision", { selector: "p" })).toBeNull();
  });
});

describe("dividers", () => {
  const line = (id: string) => `|${id}`;
  const rowsOf = (ids: string[], drawn: Record<string, boolean> = {}) =>
    drawNavRows(
      ids.map((id) =>
        id.startsWith("divider:")
          ? { id, label: "Divider" as const, divider: true as const }
          : ROWS.find((row) => row.id === id)!,
      ),
      (row) => (drawn[row.id] === false ? null : row.id),
      line,
    );

  it("draws a line only between two drawn rows", () => {
    expect(
      rowsOf(
        [
          "divider:1",
          "new-session",
          "divider:2",
          "divider:3",
          "mailbox",
          "divider:4",
          "config:tasks",
          "divider:5",
        ],
        { "config:tasks": false },
      ),
    ).toEqual(["new-session", "|divider:2", "mailbox"]);
  });

  it("adds any number from the sheet, and removing one deletes it", async () => {
    withFlags({ "customize-nav": true });
    panel();
    act(() => openCustomizeNav());
    const sheet = within(screen.getByTestId("customize-nav"));
    const addDivider = async () => {
      fireEvent.click(sheet.getByRole("button", { name: "Add to Items" }));
      fireEvent.click(await screen.findByRole("menuitem", { name: "Divider" }));
    };
    await addDivider();
    /* at the end of the list, with nothing after it, it isn't drawn */
    expect(screen.queryAllByTestId("nav-divider")).toHaveLength(0);
    fireEvent.keyDown(sheet.getAllByRole("button", { name: "Move Divider" })[0]!, {
      key: "ArrowUp",
    });
    expect(screen.getAllByTestId("nav-divider")).toHaveLength(1);
    await addDivider();
    expect(sheet.getAllByRole("button", { name: "Move Divider" })).toHaveLength(2);

    fireEvent.click(sheet.getAllByRole("button", { name: "Remove Divider" })[0]!);
    fireEvent.click(sheet.getAllByRole("button", { name: "Remove Divider" })[0]!);
    expect(sheet.queryAllByRole("button", { name: "Move Divider" })).toHaveLength(0);
    expect(screen.queryAllByTestId("nav-divider")).toHaveLength(0);
    const layout = renderHook(() => useNavLayout()).result.current;
    expect(layout.order.some((id) => id.startsWith("divider:"))).toBe(false);
    expect(Object.keys(layout.shown).some((id) => id.startsWith("divider:"))).toBe(
      false,
    );
  });
});

describe("dividers between sections", () => {
  const sessions = Array.from({ length: 3 }, (_, i) => session(i));

  it("draws one between two drawn sections, and none beside an empty one", async () => {
    withFlags({ "customize-nav": true });
    panel(sessions);
    act(() => openCustomizeNav());
    const sheet = within(screen.getByTestId("customize-nav"));
    fireEvent.click(sheet.getByRole("button", { name: /^Add to Sections/ }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Divider" }));
    fireEvent.keyDown(sheet.getByRole("button", { name: "Move Divider" }), {
      key: "ArrowUp",
    });
    /* between Pinned and Recent, with nothing pinned: no line */
    expect(screen.queryAllByTestId("nav-divider")).toHaveLength(0);

    act(() => preferences.setPinned(sessionKeyOf(sessions[0]!), true));
    expect(screen.getAllByTestId("nav-divider")).toHaveLength(1);
    expect(renderHook(() => useNavLayout()).result.current.order).toContain(
      "divider:sections:1",
    );
  });
});

describe("the expanded nav's Nodes section", () => {
  const amy = () =>
    node({
      nodeDid: "did:key:amy",
      source: "peer",
      node: { ...deployment.node, nodeDid: "did:key:amy", displayName: "Amy" },
    });
  const show = (deployments: ReturnType<typeof node>[]) =>
    renderIn(
      testApp({ deployments, selection: { nodeDid: deployment.nodeDid } }),
      <TooltipProvider>
        <NavPanel route={{ name: "sessions" }} settings={null} syncRow={false} />
      </TooltipProvider>,
    );
  const list = () => screen.queryByRole("list", { name: "Nodes", hidden: true });

  it("lists every node once added, marks the current one, and switches on a click", () => {
    withFlags({ "customize-nav": true });
    act(() => preferences.setNavLayout({ order: [], shown: { "node-list": true } }));
    show([node(), amy()]);
    const rows = within(list()!).getAllByRole("button", { hidden: true });
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveAttribute("aria-current", "true");
    fireEvent.click(within(list()!).getByRole("button", { name: /Amy/, hidden: true }));
    expect(
      within(list()!).getByRole("button", { name: /Amy/, hidden: true }),
    ).toHaveAttribute("aria-current", "true");
  });

  it("isn't drawn while the only node is this machine's, nor until added", () => {
    withFlags({ "customize-nav": true });
    act(() => preferences.setNavLayout({ order: [], shown: { "node-list": true } }));
    const { unmount } = show([node()]);
    expect(list()).toBeNull();
    unmount();
    act(() => preferences.resetNavLayout());
    show([node(), amy()]);
    expect(list()).toBeNull();
  });
});
