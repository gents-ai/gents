/* Notifications from a pack's page: the kinds its module declares, the
   owner that decides whether one is told, the switches in Settings, and
   where a click leads. */
import { act, fireEvent, render, screen } from "@testing-library/react";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { loadRuntimeUxPlugin, retireRuntimeUxPlugin } from "@/contrib/loader";
import { parseNotificationKinds } from "@/contrib/notification-kinds";
import { installUxHost, type UxHost, type UxNotifyCaller } from "@/contrib/plugin";
import {
  publishUxPlugin,
  resetUxPluginStore,
  uxPluginRecords,
} from "@/contrib/plugins-store";
import { PLUGIN_PAGE_AREA } from "@/contrib/types";
import { parseLink } from "@/lib/links";
import {
  createPluginNotifier,
  NOTIFY_RATE_COUNT,
  NOTIFY_RATE_WINDOW,
  type PluginNotifyDeps,
} from "@/lib/notify/plugins";
import { clickRouter, type Notice, type NotifyPlatform } from "@/lib/notify/platform";
import { createTeller } from "@/lib/notify/watch";
import {
  bindNav,
  navigate,
  openLink,
  parseRoute,
  pathFor,
  type Route,
} from "@/lib/router";
import { preferences, resetPreferences } from "@/preferences";
import { withFlags } from "./app-fixture";
import {
  installInProcessSandbox,
  settle,
  uninstallInProcessSandbox,
} from "./sandbox-fixture";

const BOARD: UxNotifyCaller = {
  id: "board",
  name: "Board",
  key: "acme/kit/board",
  kinds: [{ kind: "task-assigned", label: "Task assigned" }],
};

describe("the kinds a plugin's module declares", () => {
  it("takes plain kinds with a label, and none when absent", () => {
    expect(parseNotificationKinds(undefined)).toEqual({ kinds: [] });
    expect(
      parseNotificationKinds([
        {
          kind: "task-assigned",
          label: "  Task\nassigned ",
          description: "A task is yours.",
        },
      ]),
    ).toEqual({
      kinds: [
        {
          kind: "task-assigned",
          label: "Task assigned",
          description: "A task is yours.",
        },
      ],
    });
  });

  it("refuses a kind that isn't plain, twice declared, unlabelled or too many", () => {
    for (const bad of [
      "task",
      [{ kind: "Task", label: "x" }],
      [{ kind: "a:b", label: "x" }],
      [
        { kind: "a", label: "x" },
        { kind: "a", label: "y" },
      ],
      [{ kind: "a" }],
      [{ kind: "a", label: "x".repeat(61) }],
      [{ kind: "a", label: "x", description: "" }],
      Array.from({ length: 9 }, (_, i) => ({ kind: `k${i}`, label: "x" })),
    ])
      expect(parseNotificationKinds(bad), JSON.stringify(bad)).toHaveProperty("error");
  });
});

describe("the owner of plugins' notifications", () => {
  let enabled: boolean;
  let speaks: boolean;
  let focused: boolean;
  let switched: Set<string>;
  let now: number;
  let told: (Notice & { to: Route })[];
  let shownInApp: { notice: Notice; open: () => void }[];
  let opened: Route[];
  let offered: string[] = [];

  const notifier = () =>
    createPluginNotifier({
      enabled: () => enabled,
      speaks: () => speaks,
      switchedOn: (key, kind) => switched.has(`${key} ${kind}`),
      focused: () => focused,
      now: () => now,
      tell: (notice) => void told.push(notice),
      show: (notice, open) => void shownInApp.push({ notice, open }),
      open: (to) => void opened.push(to),
      offer: (_caller, { kind }) => void offered.push(kind),
    } satisfies PluginNotifyDeps);

  beforeEach(() => {
    enabled = true;
    speaks = true;
    focused = false;
    switched = new Set(["acme/kit/board task-assigned"]);
    now = 0;
    told = [];
    shownInApp = [];
    opened = [];
    offered = [];
  });

  it("tells a switched-on kind, attributed, with a click that opens the plugin's page", async () => {
    const notify = notifier();
    await notify(BOARD, { kind: "task-assigned", title: "Ship the release" });
    await notify(BOARD, {
      kind: "task-assigned",
      title: "Review\nthe notes",
      body: "Due Friday",
      target: "tasks/42",
    });
    expect(told).toEqual([
      {
        title: "Board: Ship the release",
        to: { name: "plugin", pluginId: "board" },
      },
      {
        title: "Board: Review the notes",
        body: "Due Friday",
        to: { name: "plugin", pluginId: "board", path: "tasks/42" },
      },
    ]);
  });

  it("refuses an undeclared kind", async () => {
    await expect(notifier()(BOARD, { kind: "deploy", title: "x" })).rejects.toThrow(
      'board: ctx.notify: kind "deploy" is not declared (declared: task-assigned)',
    );
    expect(told).toEqual([]);
  });

  it("refuses while the experimental features are off, and outside the main window", async () => {
    enabled = false;
    await expect(
      notifier()(BOARD, { kind: "task-assigned", title: "x" }),
    ).rejects.toThrow(/notifications from packs are off/);
    enabled = true;
    speaks = false;
    await expect(
      notifier()(BOARD, { kind: "task-assigned", title: "x" }),
    ).rejects.toThrow(/only the app's main window/);
    expect(told).toEqual([]);
  });

  it("refuses a kind the person hasn't turned on, for this pack's plugin only, and offers it once", async () => {
    switched = new Set();
    const notify = notifier();
    await expect(notify(BOARD, { kind: "task-assigned", title: "x" })).rejects.toThrow(
      /"Task assigned" notifications are off/,
    );
    await expect(notify(BOARD, { kind: "task-assigned", title: "y" })).rejects.toThrow(
      /are off/,
    );
    expect(offered).toEqual(["task-assigned"]);
    switched = new Set(["other/pack/board task-assigned"]);
    await expect(
      notifier()(BOARD, { kind: "task-assigned", title: "x" }),
    ).rejects.toThrow(/are off/);
    expect(told).toEqual([]);
  });

  it("refuses a target outside the plugin's own page", async () => {
    const notify = notifier();
    for (const target of [
      "../other",
      "/plugins/other",
      "gents://plugin/other",
      "https://example.com",
      "a/./b",
      "a//b",
      "a?b=1",
      "a#b",
      7,
      "x".repeat(201),
    ])
      await expect(
        notify(BOARD, { kind: "task-assigned", title: "x", target }),
        String(target),
      ).rejects.toThrow(/target must be a place within the plugin's own page/);
    expect(told).toEqual([]);
  });

  it("refuses a malformed request and text past the limits", async () => {
    const notify = notifier();
    for (const request of [
      null,
      "hello",
      [],
      { kind: "task-assigned" },
      { kind: "task-assigned", title: " \n " },
      { kind: "task-assigned", title: "x".repeat(81) },
      { kind: "task-assigned", title: "x", body: 3 },
      { kind: "task-assigned", title: "x", body: "y".repeat(161) },
    ])
      await expect(notify(BOARD, request), JSON.stringify(request)).rejects.toThrow(
        /ctx\.notify/,
      );
    expect(told).toEqual([]);
  });

  it("holds each plugin to its rate, and lets it speak again once a minute has passed", async () => {
    const notify = notifier();
    const ask = () => notify(BOARD, { kind: "task-assigned", title: "x" });
    for (let i = 0; i < NOTIFY_RATE_COUNT; i += 1) await ask();
    await expect(ask()).rejects.toThrow(/at most 5 notifications a minute/);
    expect(told).toHaveLength(NOTIFY_RATE_COUNT);
    const other = { ...BOARD, key: "acme/kit/other" };
    switched.add("acme/kit/other task-assigned");
    await notify(other, { kind: "task-assigned", title: "y" });
    expect(told).toHaveLength(NOTIFY_RATE_COUNT + 1);
    now += NOTIFY_RATE_WINDOW;
    await ask();
    expect(told).toHaveLength(NOTIFY_RATE_COUNT + 2);
  });

  it("shows it in the app while the window is in front, with Open going to the page", async () => {
    focused = true;
    await notifier()(BOARD, { kind: "task-assigned", title: "x", target: "tasks/4" });
    expect(told).toEqual([]);
    expect(shownInApp.map((s) => s.notice)).toEqual([{ title: "Board: x" }]);
    shownInApp[0]!.open();
    expect(opened).toEqual([{ name: "plugin", pluginId: "board", path: "tasks/4" }]);
  });
});

describe("a click on a notification", () => {
  const notice = {
    title: "Board: x",
    to: { name: "plugin", pluginId: "board" } as Route,
  };

  const teller = (platform: NotifyPlatform) => {
    const opened: Route[] = [];
    const offered: { title: string; open: () => void }[] = [];
    let back = () => {};
    const { tell } = createTeller({
      platform,
      now: () => 0,
      open: (to) => void opened.push(to),
      offer: (o, open) => void offered.push({ title: o.title, open }),
      onFocus: (then) => {
        back = then;
        return () => {};
      },
    });
    return { tell, opened, offered, back: () => back() };
  };

  it("opens what it is about where the platform reports clicks, offering nothing", async () => {
    let shown: Notice | null = null;
    const t = teller({
      permission: async () => "granted",
      request: async () => "granted",
      show: async (n) => void (shown = n),
      reportsClicks: true,
    });
    t.tell(notice);
    await settle();
    t.back();
    expect(t.offered).toEqual([]);
    (shown as Notice | null)?.onClick?.();
    expect(t.opened).toEqual([{ name: "plugin", pluginId: "board" }]);
  });

  it("falls back to offering it in the app when this one notice's click won't be heard", async () => {
    const t = teller({
      permission: async () => "granted",
      request: async () => "granted",
      show: async () => ({ reportsClick: false }),
      reportsClicks: true,
    });
    t.tell(notice);
    await settle();
    t.back();
    expect(t.offered.map((o) => o.title)).toEqual(["Board: x"]);
    t.offered[0]!.open();
    expect(t.opened).toEqual([{ name: "plugin", pluginId: "board" }]);
  });

  it("reaches the callback it was posted with once, by the id the shell reports", async () => {
    let heard: (id: string) => void = () => {};
    const router = clickRouter(async (h) => {
      heard = h;
    });
    const first = vi.fn();
    const second = vi.fn();
    const a = await router.expect(first);
    const b = await router.expect(second);
    expect(a).not.toBe(b);
    heard(b);
    heard(b);
    heard("unknown");
    expect(second).toHaveBeenCalledTimes(1);
    expect(first).not.toHaveBeenCalled();
    router.forget(a);
    heard(a);
    expect(first).not.toHaveBeenCalled();
  });
});

describe("a plugin's page as a place to go", () => {
  function mountNav() {
    let nav: Nav | null = null;
    function Bind() {
      nav = useNav();
      bindNav(nav);
      return null;
    }
    render(
      <MemoryNavProvider>
        <Bind />
      </MemoryNavProvider>,
    );
    return () => nav!;
  }
  afterEach(() => bindNav(null));

  it("is a link, optionally to a place within the page", () => {
    expect(parseLink("gents://plugin/board")).toEqual({
      kind: "plugin",
      pluginId: "board",
    });
    expect(parseLink("gents:plugin/board/tasks/42")).toEqual({
      kind: "plugin",
      pluginId: "board",
      path: "tasks/42",
    });
    for (const href of [
      "gents://plugin/",
      "gents://plugin/Board",
      "gents://plugin/board/../x",
      "gents://plugin/board/%2E%2E/x",
      "gents://plugin/board/a%2Fb",
    ])
      expect(parseLink(href), href).toBeNull();
  });

  it("keeps the place through the route's path", () => {
    const route: Route = { name: "plugin", pluginId: "board", path: "tasks/42" };
    expect(pathFor(route)).toBe("/plugins/board/tasks/42");
    expect(parseRoute(pathFor(route))).toEqual(route);
    expect(parseRoute("/plugins/board")).toEqual({ name: "plugin", pluginId: "board" });
  });

  it("is where a link goes, as a notification's click does", () => {
    const nav = mountNav();
    act(() => void openLink("gents://plugin/board/tasks/42"));
    expect(nav().route).toEqual({
      name: "plugin",
      pluginId: "board",
      path: "tasks/42",
    });
    act(() => navigate({ name: "plugin", pluginId: "board" }));
    expect(nav().route).toEqual({ name: "plugin", pluginId: "board" });
  });
});

describe("a plugin that notifies, in its frame", () => {
  const asked: { caller: UxNotifyCaller; request: unknown }[] = [];
  const host: UxHost = {
    shell: {
      route: () => ({ name: "sessions" }),
      navigate: () => {},
      deployments: () => [],
      selectedDeployment: () => null,
      selectedSessionId: () => null,
      send: async () => {},
    },
    os: {
      openExternal: async () => false,
      revealPath: async () => false,
      pickDirectory: async () => null,
    },
    notify: async (caller, request) => {
      asked.push({ caller, request });
      if ((request as { kind?: string }).kind !== "task-assigned")
        throw new Error("board: ctx.notify: not declared");
    },
  };

  const module = (notifications: string) => `
import { PLUGIN_PAGE_AREA } from '@gents/ux-sdk'
export default {
  id: 'board',
  name: 'Board',
  notifications: ${notifications},
  register(ctx) {
    ctx.register({ id: 'page', area: PLUGIN_PAGE_AREA, render: () => null })
    ctx.notify({ kind: 'task-assigned', title: 'New task', target: 'tasks/4' })
      .then(() => ctx.notify({ kind: 'deploy', title: 'x' }))
      .then(() => ctx.storage.set('told', 'all'), (e) => ctx.storage.set('told', e.message))
  },
}`;

  const load = (source: string) =>
    loadRuntimeUxPlugin(
      { source },
      {
        origin: "acme/kit/board",
        expectId: "board",
        door: "pack",
        pack: "acme/kit",
        file: "/packs/acme/kit/ux/board/plugin.js",
        listing: "acme/kit/board",
        declared: { areas: [PLUGIN_PAGE_AREA], directives: [] },
      },
    );

  beforeEach(() => {
    asked.length = 0;
    installUxHost(host);
    resetUxPluginStore();
    installInProcessSandbox();
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
  });
  afterEach(() => {
    for (const id of Object.keys(uxPluginRecords())) retireRuntimeUxPlugin(id);
    uninstallInProcessSandbox();
    vi.restoreAllMocks();
  });

  it("asks under its pack's listing with its declared kinds, and hears each refusal", async () => {
    expect(
      await load(module(`[{ kind: 'task-assigned', label: 'Task assigned' }]`)),
    ).toBe("board");
    await settle(12);
    expect(uxPluginRecords().board?.notifications).toEqual([
      { kind: "task-assigned", label: "Task assigned" },
    ]);
    expect(asked).toEqual([
      {
        caller: {
          id: "board",
          name: "Board",
          key: "acme/kit/board",
          kinds: [{ kind: "task-assigned", label: "Task assigned" }],
        },
        request: { kind: "task-assigned", title: "New task", target: "tasks/4" },
      },
      expect.objectContaining({ request: { kind: "deploy", title: "x" } }),
    ]);
    expect(window.localStorage.getItem("gents.ux.board.told")).toBe(
      '"board: ctx.notify: not declared"',
    );
  });

  it("fails to load with a malformed declaration, on its row", async () => {
    expect(await load(module(`[{ kind: 'Task', label: 'x' }]`))).toBeNull();
    expect(uxPluginRecords()["acme/kit/board"]).toMatchObject({
      status: "error",
      error: expect.stringContaining("notification kind"),
    });
    expect(asked).toEqual([]);
  });
});

describe("the switches for plugins' notifications", () => {
  beforeEach(() => {
    resetUxPluginStore();
    publishUxPlugin({
      id: "board",
      name: "Board",
      door: "pack",
      pack: "acme/kit",
      status: "loaded",
      notifications: [
        {
          kind: "task-assigned",
          label: "Task assigned",
          description: "A task is yours.",
        },
      ],
    });
    publishUxPlugin({ id: "quiet", name: "Quiet", door: "pack", status: "loaded" });
  });
  afterEach(() => {
    resetUxPluginStore();
    act(() => resetPreferences());
  });

  it("list each declared kind, off until the person turns it on", async () => {
    withFlags({ notifications: true, "pack-notifications": true });
    const { NotificationsGroup } =
      await import("../src/ui/screens/settings/NotificationsGroup");
    render(<NotificationsGroup />);
    const toggle = screen.getByRole("switch", { name: "Task assigned" });
    expect(toggle).not.toBeChecked();
    expect(screen.getByText("A task is yours.")).toBeInTheDocument();
    /* grouped under the plugin, with its pack beside the name */
    expect(screen.getByText("Board")).toBeInTheDocument();
    expect(screen.getByText("acme/kit")).toBeInTheDocument();
    expect(screen.queryByRole("switch", { name: /^Quiet/ })).toBeNull();
    fireEvent.click(toggle);
    expect(screen.getByRole("switch", { name: "Task assigned" })).toBeChecked();
    const { pluginNotifyOn } = await import("../src/ui/preferences");
    expect(pluginNotifyOn("acme/kit/board", "task-assigned")).toBe(true);
    expect(pluginNotifyOn("other/kit/board", "task-assigned")).toBe(false);
    act(() => preferences.setPluginNotify("acme/kit/board", "task-assigned", false));
    expect(screen.getByRole("switch", { name: "Task assigned" })).not.toBeChecked();
  });

  it("stay hidden until notifications from packs are on", async () => {
    withFlags({ notifications: true });
    const { NotificationsGroup } =
      await import("../src/ui/screens/settings/NotificationsGroup");
    render(<NotificationsGroup />);
    expect(screen.getByRole("switch", { name: "Finished" })).toBeInTheDocument();
    expect(screen.queryByRole("switch", { name: "Task assigned" })).toBeNull();
  });
});
