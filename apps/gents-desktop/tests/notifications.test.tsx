import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createStore } from "zustand/vanilla";
import type { MailboxItemView, SessionSummary } from "@source-inc/gents-desktop-client";

import { sessionKeyOf, type FleetState } from "../src/hooks/fleetStore";
import { changesBetween, type NotifyKind } from "../src/ui/lib/notify/events";
import type { NotifyPlatform } from "../src/ui/lib/notify/platform";
import {
  BURST_MOST,
  BURST_QUIET,
  FOLLOW_WITHIN,
  watchNotifications,
} from "../src/ui/lib/notify/watch";
import { preferences, resetPreferences } from "../src/ui/preferences";

const NODE = "did:key:node";
const START = Date.parse("2026-11-02T12:00:00Z");

const session = (
  sessionId: string,
  turnState: string,
  latestRequestId = "r1",
  more: Partial<SessionSummary> = {},
) =>
  ({
    sessionId,
    nodeDid: NODE,
    requesterDid: null,
    title: `Session ${sessionId}`,
    turnState,
    latestRequestId,
    startedBy: null,
    ...more,
  }) as SessionSummary;

const item = (itemId: string, more: Partial<MailboxItemView> = {}) =>
  ({
    itemId,
    itemKey: itemId,
    nodeDid: NODE,
    status: "open",
    kind: "finished",
    title: `Item ${itemId}`,
    payload: null,
    sessionId: null,
    targetNodeDid: NODE,
    createdAt: new Date(START + 1000).toISOString(),
    ...more,
  }) as MailboxItemView;

const question = (itemId: string, sessionId: string) =>
  item(itemId, {
    kind: "ask",
    action: "start_request",
    sourceKind: "session",
    sourceId: sessionId,
    requestId: "request",
    targetAgentId: "default",
    sessionId,
    payload: JSON.stringify({
      version: 1,
      prompt: "Which one?",
      options: [
        { id: "a", label: "A" },
        { id: "b", label: "B" },
      ],
      multi_select: false,
      allow_free_text: true,
    }),
  });

const fleetOf = (
  sessions: SessionSummary[],
  items: MailboxItemView[] = [],
): Pick<FleetState, "sessions" | "mailboxOf"> => ({
  sessions: Object.fromEntries(sessions.map((s) => [sessionKeyOf(s), s])),
  mailboxOf: { [NODE]: items },
});

describe("what the notifications tell", () => {
  const since = new Date(START).toISOString();

  it("says nothing about what had already happened when the view was first seen", () => {
    const before = fleetOf([]);
    const after = fleetOf(
      [session("a", "completed")],
      [item("old", { createdAt: new Date(START - 1000).toISOString() })],
    );
    expect(changesBetween(before, after, since)).toEqual([]);
  });

  it("tells a session that finished or failed, once per request", () => {
    const running = fleetOf([session("a", "running"), session("b", "running")]);
    const ended = fleetOf([session("a", "completed"), session("b", "failed")]);
    const events = changesBetween(running, ended, since);
    expect(events.map((e) => [e.kind, e.title])).toEqual([
      ["finished", "Session a"],
      ["failed", "Session b"],
    ]);
    expect(changesBetween(ended, ended, since)).toEqual([]);
    const again = fleetOf([session("a", "completed", "r2"), session("b", "failed")]);
    expect(changesBetween(ended, again, since).map((e) => e.key)).toHaveLength(1);
  });

  it("tells a worker only when it failed, and nothing for a stopped turn", () => {
    const worker = { startedBy: { sessionId: "p" } } as Partial<SessionSummary>;
    const before = fleetOf([
      session("w1", "running", "r1", worker),
      session("w2", "running", "r1", worker),
      session("s", "running"),
    ]);
    const after = fleetOf([
      session("w1", "completed", "r1", worker),
      session("w2", "failed", "r1", worker),
      session("s", "interrupted"),
    ]);
    const events = changesBetween(before, after, since);
    expect(events).toHaveLength(1);
    expect(events[0]).toMatchObject({ kind: "failed", worker: true });
  });

  it("tells a question that arrived, and a new mailbox item", () => {
    const before = fleetOf([session("a", "running")]);
    const after = fleetOf([session("a", "running")], [question("q", "a"), item("m")]);
    const events = changesBetween(before, after, since);
    expect(events.map((e) => [e.kind, e.title, e.to])).toEqual([
      ["question", "Session a", { name: "session", sessionId: "a", nodeDid: NODE }],
      ["mailbox", "Item m", { name: "mailbox" }],
    ]);
  });
});

describe("the notification owner", () => {
  let now = START;
  let focused = false;
  let visible = false;
  let shown: { title: string; body?: string; onClick?: () => void }[] = [];
  let opened: unknown[] = [];
  let focusHandler: (() => void) | null = null;
  let kinds: Record<NotifyKind, boolean>;
  let enabled = true;

  const platform = (reportsClicks: boolean): NotifyPlatform => ({
    permission: async () => "granted",
    request: async () => "granted",
    show: (n) => void shown.push(n),
    reportsClicks,
  });

  const start = (initial: SessionSummary[], reportsClicks = false) => {
    const fleet = createStore(() => fleetOf(initial) as FleetState);
    const selection = createStore(() => ({
      nodeDid: NODE as string | null,
      sessionId: null as string | null,
    }));
    const stop = watchNotifications(
      { fleet: fleet as never, selection: selection as never },
      {
        platform: platform(reportsClicks),
        now: () => now,
        focused: () => focused,
        visible: () => visible,
        enabled: () => enabled,
        kinds: () => kinds,
        open: (to) => void opened.push(to),
        onFocus: (then) => {
          focusHandler = then;
          return () => {};
        },
      },
    );
    const set = (sessions: SessionSummary[], items: MailboxItemView[] = []) =>
      fleet.setState(fleetOf(sessions, items) as FleetState, true);
    return { set, selection, stop };
  };

  const pass = (ms: number) => {
    now += ms;
    vi.advanceTimersByTime(ms);
  };

  beforeEach(() => {
    vi.useFakeTimers();
    now = START;
    focused = false;
    visible = false;
    shown = [];
    opened = [];
    focusHandler = null;
    enabled = true;
    kinds = { finished: true, failed: true, question: true, mailbox: true };
  });
  afterEach(() => vi.useRealTimers());

  it("tells a finished session after the burst settles", () => {
    const { set } = start([session("a", "running")]);
    set([session("a", "completed")]);
    expect(shown).toEqual([]);
    pass(BURST_QUIET);
    expect(shown.map((n) => n.title)).toEqual(["Finished: Session a"]);
  });

  it("tells a burst of one kind as one notification", () => {
    const { set } = start(
      ["a", "b", "c", "d", "e"].map((id) => session(id, "running")),
    );
    const done = (ids: string[]) =>
      ["a", "b", "c", "d", "e"].map((id) =>
        session(id, ids.includes(id) ? "completed" : "running"),
      );
    set(done(["a"]));
    pass(1000);
    set(done(["a", "b", "c", "d"]));
    pass(BURST_QUIET);
    expect(shown).toHaveLength(1);
    expect(shown[0]).toMatchObject({
      title: "4 sessions finished",
      body: "Session a, Session b, Session c, and 1 more",
    });
  });

  it("tells a burst that keeps going after a while anyway", () => {
    const ids = Array.from({ length: 10 }, (_, i) => `s${i}`);
    const { set } = start(ids.map((id) => session(id, "running")));
    for (let i = 0; i < ids.length; i += 1) {
      set(ids.map((id, j) => session(id, j <= i ? "completed" : "running")));
      pass(BURST_QUIET - 500);
      if (now - START >= BURST_MOST) break;
    }
    expect(shown.length).toBeGreaterThan(0);
  });

  it("stays quiet while the window is in front, and about the session on screen", () => {
    const { set, selection } = start([
      session("a", "running"),
      session("b", "running"),
    ]);
    focused = true;
    visible = true;
    set([session("a", "completed"), session("b", "running")]);
    pass(BURST_QUIET);
    expect(shown).toEqual([]);
    focused = false;
    selection.setState({ nodeDid: NODE, sessionId: "b" });
    set([session("a", "completed"), session("b", "completed")]);
    pass(BURST_QUIET);
    expect(shown).toEqual([]);
  });

  it("follows each kind's switch, and the experimental feature", () => {
    kinds.finished = false;
    const { set } = start([session("a", "running"), session("b", "running")]);
    set([session("a", "completed"), session("b", "failed")]);
    pass(BURST_QUIET);
    expect(shown.map((n) => n.title)).toEqual(["Failed: Session b"]);
    enabled = false;
    set([session("a", "completed", "r2"), session("b", "failed", "r2")]);
    pass(BURST_QUIET);
    expect(shown).toHaveLength(1);
  });

  it("tells a question and a mailbox item that arrived", () => {
    const { set } = start([session("a", "running")]);
    set([session("a", "running")], [question("q", "a"), item("m")]);
    pass(BURST_QUIET);
    expect(shown.map((n) => n.title)).toEqual([
      "Needs your answer: Session a",
      "In your mailbox: Item m",
    ]);
  });

  it("opens what a clicked notification is about", () => {
    const { set } = start([session("a", "running")], true);
    set([session("a", "completed")]);
    pass(BURST_QUIET);
    shown[0]!.onClick?.();
    expect(opened).toEqual([{ name: "session", sessionId: "a", nodeDid: NODE }]);
  });

  it("without click callbacks, opens it when the window comes back soon after", () => {
    const { set } = start([session("a", "running"), session("b", "running")]);
    set([session("a", "completed"), session("b", "running")]);
    pass(BURST_QUIET);
    pass(1000);
    focusHandler?.();
    expect(opened).toEqual([{ name: "session", sessionId: "a", nodeDid: NODE }]);
    set([session("a", "completed"), session("b", "completed")]);
    pass(BURST_QUIET);
    pass(FOLLOW_WITHIN + 1);
    focusHandler?.();
    expect(opened).toHaveLength(1);
  });
});

describe("the notification settings", () => {
  beforeEach(() => resetPreferences({ notifications: "on" }));
  afterEach(() => act(() => resetPreferences()));

  it("switch each kind on this device", async () => {
    const { NotificationsGroup } =
      await import("../src/ui/screens/settings/NotificationsGroup");
    render(<NotificationsGroup />);
    const finished = screen.getByRole("switch", { name: "Finished" });
    expect(finished).toBeChecked();
    fireEvent.click(finished);
    expect(screen.getByRole("switch", { name: "Finished" })).not.toBeChecked();
    act(() => preferences.setNotify("finished", true));
    expect(screen.getByRole("switch", { name: "Finished" })).toBeChecked();
  });

  it("have their own Settings section while the feature is on, and General otherwise", async () => {
    const { SettingsScreen } =
      await import("../src/ui/screens/settings/SettingsScreen");
    const { renderIn, testApp } = await import("./app-fixture");
    const view = renderIn(testApp(), <SettingsScreen section="notifications" />);
    expect(screen.getByRole("switch", { name: "Finished" })).toBeInTheDocument();
    view.unmount();
    act(() => resetPreferences());
    renderIn(testApp(), <SettingsScreen section="notifications" />);
    expect(screen.queryByRole("switch", { name: "Finished" })).toBeNull();
    expect(screen.queryByRole("link", { name: /Notifications/ })).toBeNull();
    expect(screen.getAllByText("General").length).toBeGreaterThan(0);
  });
});
