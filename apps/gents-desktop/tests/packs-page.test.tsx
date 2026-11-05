import {
  cleanup,
  act,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DeploymentView, InstalledPack } from "@source-inc/gents-desktop-client";
import {
  publishUxPlugin,
  resetUxPluginStore,
  uxDecisions,
  uxPluginRecords,
} from "../src/ui/contrib/plugins-store";
import { registry } from "../src/ui/contrib/registry";
import { PLUGIN_PAGE_AREA } from "../src/ui/contrib/types";
import { parseRoute, pathFor } from "../src/ui/lib/router";
import { resetPreferences } from "../src/ui/preferences";
import { PacksScreen } from "../src/ui/screens/PacksScreen";
import { openSearch, SearchPalette } from "../src/ui/screens/SearchPalette";
import {
  byAttention,
  localPlugins,
  partLabel,
  partsOf,
  resetInstalledPacks,
  statusOf,
  resourceTree,
  rememberPacksQuery,
  rememberPacksView,
  rememberPacksAuthor,
  showPublisher,
  publisherOf,
} from "../src/ui/screens/packs/model";
import { renderIn, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
}));

const RESEARCH = "demo/research";
const tag = ["gents:pack:research"];
const node: DeploymentView = {
  ...deployment,
  agents: [
    ...deployment.agents,
    { ...deployment.agents[0]!, agentId: "r1", tags: tag },
    { ...deployment.agents[0]!, agentId: "r2", tags: tag },
  ],
  skills: deployment.skills.map((s) => ({ ...s, tags: tag })),
};
const research: InstalledPack = {
  pack: RESEARCH,
  installed: "1.0.0",
  latest: "1.1.0",
  outdated: true,
  error: null,
};

let undo: (() => void) | null = null;
function plugins() {
  publishUxPlugin(
    { id: "demo.board", name: "Board", door: "pack", pack: RESEARCH, status: "loaded" },
    { activate: vi.fn(), deactivate: vi.fn() },
  );
  publishUxPlugin({ id: "mine", name: "My panel", door: "dev", status: "loaded" });
  undo = registry.register({
    id: "demo.board:page",
    area: PLUGIN_PAGE_AREA,
    source: "plugin:demo.board",
    title: "Board",
    render: () => null,
  });
}

beforeEach(() => resetInstalledPacks());
afterEach(() => {
  undo?.();
  undo = null;
  resetUxPluginStore();
  resetPreferences();
});

function show(item?: string, api: Record<string, unknown> = {}) {
  withFlags({ packs: true });
  rememberPacksView("installed");
  rememberPacksQuery("");
  rememberPacksAuthor(null);
  plugins();
  const app = testApp({
    api: {
      listInstalledPacks: async () => ({ packs: [research] }),
      packInfo: async () => ({
        description: "Searches the web and writes a cited summary.",
        latest: "1.1.0",
        versions: [
          { version: "1.0.0", published_at: "2026-10-01T00:00:00Z" },
          { version: "1.1.0", published_at: "2026-11-01T00:00:00Z" },
        ],
      }),
      searchPacks: async () => ({
        packs: [
          { namespace: "demo", name: "research", description: "Research" },
          { namespace: "acme", name: "docs", description: "A documents page" },
        ],
        has_more: false,
      }),
      ...api,
    },
    snapshot: { bootstrap, client: { deployments: [node] } },
  });
  renderIn(app, <PacksScreen item={item} />);
  return app;
}

describe("a pack's parts", () => {
  it("groups what a pack added by kind, its plugins' pages last", () => {
    plugins();
    const pages = registry.getArea(PLUGIN_PAGE_AREA);
    expect(partsOf(node, RESEARCH, uxPluginRecords(), pages).map(partLabel)).toEqual([
      "2 agents",
      `${deployment.skills.length} ${deployment.skills.length === 1 ? "skill" : "skills"}`,
      "1 page",
    ]);
  });

  it("names plugins from the home folder as local, and a pack's update as available", () => {
    plugins();
    expect(localPlugins(uxPluginRecords()).map((p) => p.id)).toEqual(["mine"]);
    expect(statusOf(research)).toEqual({ text: "1.0.0 · 1.1.0 available", warn: true });
  });
});

describe("the installed list's order", () => {
  it("puts packs with an update first, then sorts by the name the row shows", () => {
    const pack = (name: string, outdated = false): InstalledPack => ({
      pack: name,
      installed: "1.0.0",
      latest: outdated ? "1.1.0" : "1.0.0",
      outdated,
      error: null,
    });
    const order = [
      pack("zed/alpha"),
      pack("demo/tasks", true),
      pack("acme/board"),
      pack("demo/research", true),
    ]
      .sort(byAttention)
      .map((p) => p.pack);
    expect(order).toEqual(["demo/research", "demo/tasks", "zed/alpha", "acme/board"]);
  });
});

describe("what a pack added, as it fits together", () => {
  it("nests the tasks that run an agent and the tools its context gives it", () => {
    const agent = { ...deployment.agents[0]!, agentId: "a1", contextId: "c1" };
    const fit = {
      ...deployment,
      agents: [agent],
      contexts: [{ node_did: deployment.nodeDid, context_id: "c1", tools_id: "t1" }],
      tasks: [{ ...deployment.tasks[0]!, taskId: "k1", agentId: "a1" }],
    } as typeof deployment;
    const parts = [
      {
        one: "agent",
        many: "agents",
        count: 1,
        section: "agents",
        items: [{ id: "a1", name: "Worker" }],
      },
      {
        one: "task",
        many: "tasks",
        count: 1,
        section: "tasks",
        items: [{ id: "k1", name: "Run" }],
      },
      {
        one: "tool set",
        many: "tool sets",
        count: 2,
        section: "tools",
        items: [
          { id: "t1", name: "Its tools" },
          { id: "t2", name: "Spare" },
        ],
      },
    ];
    const tree = resourceTree(fit, parts);
    expect(
      tree.map((r) => [r.name, r.children.map((c) => `${c.name} (${c.relation})`)]),
    ).toEqual([
      ["Worker", ["Run (runs it)", "Its tools (it uses)"]],
      ["Spare", []],
    ]);
  });
});

describe("who published a pack", () => {
  it("says you, someone by name, or nothing when the registry has no word", () => {
    expect(publisherOf({ owner: "dave" }, "dave")).toEqual({
      name: "dave",
      yours: true,
    });
    expect(publisherOf({ owner: "gents" }, "dave")).toEqual({
      name: "gents",
      yours: false,
    });
    expect(publisherOf({ owner: "gents" }, null)).toEqual({
      name: "gents",
      yours: false,
    });
    expect(publisherOf("unread", "dave")).toBeNull();
    expect(publisherOf({}, "dave")).toBeNull();
  });
});

describe("the Packs page", () => {
  it("lists the node's packs with their update, then the registry's others when browsing", async () => {
    show();
    expect(screen.getByRole("heading", { name: "Packs" })).toBeInTheDocument();
    const installed = await screen.findByRole("list", { name: "Installed packs" });
    const card = within(installed).getByRole("link", { name: "research" });
    expect(card).toHaveAttribute("href", "/packs/demo%2Fresearch");
    expect(within(installed).getByText(/2 agents/)).toBeInTheDocument();
    expect(
      within(installed).getByRole("button", { name: "Update" }),
    ).toBeInTheDocument();
    const local = screen.getByRole("region", { name: "Local plugins" });
    expect(within(local).getByText("My panel")).toBeInTheDocument();
    expect(within(local).getByText("On this device")).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Registry packs" })).toBeNull();
    fireEvent.click(screen.getByRole("tab", { name: "Registry" }));
    const registry = await screen.findByRole("list", { name: "Registry packs" });
    expect(await within(registry).findByText("docs")).toBeInTheDocument();
    expect(within(registry).getAllByRole("button", { name: "Install" })).toHaveLength(
      1,
    );
  });

  it("returns to the list and search it left, while the app runs", async () => {
    const app = show();
    fireEvent.click(await screen.findByRole("tab", { name: "Registry" }));
    fireEvent.change(screen.getByRole("textbox", { name: "Search packs" }), {
      target: { value: "doc" },
    });
    cleanup();
    withFlags({ packs: true });
    renderIn(app, <PacksScreen />);
    expect(screen.getByRole("tab", { name: "Registry" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByRole("textbox", { name: "Search packs" })).toHaveValue("doc");
  });

  it("narrows the registry to one publisher's packs, until its chip is cleared", async () => {
    show();
    act(() => showPublisher("acme"));
    const registry = await screen.findByRole("list", { name: "Registry packs" });
    expect(await within(registry).findByText("docs")).toBeInTheDocument();
    expect(within(registry).queryByText("research")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /not only acme/ }));
    expect(await within(registry).findByText("research")).toBeInTheDocument();
  });

  it("offers a pack's actions from its row's menu", async () => {
    show();
    const installed = await screen.findByRole("list", { name: "Installed packs" });
    await act(async () => {
      fireEvent.click(
        within(installed).getByRole("button", { name: `More for ${RESEARCH}` }),
      );
    });
    expect(
      (await screen.findAllByRole("menuitem")).map((item) => item.textContent),
    ).toEqual(["Open", "Update", "Remove"]);
  });

  it("points a search of your packs at the registry's matches", async () => {
    show();
    await screen.findByRole("list", { name: "Installed packs" });
    fireEvent.change(screen.getByRole("textbox", { name: "Search packs" }), {
      target: { value: "doc" },
    });
    expect(screen.queryByRole("list", { name: "Registry packs" })).toBeNull();
    fireEvent.click(
      await screen.findByRole("button", { name: /1 more in the registry/ }),
    );
    const registry = await screen.findByRole("list", { name: "Registry packs" });
    expect(await within(registry).findByText("docs")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Registry" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("opens a pack on its own page: what it adds, its pages, its plugins and versions", async () => {
    show(RESEARCH);
    expect(
      await screen.findByRole("heading", { name: "research" }),
    ).toBeInTheDocument();
    const adds = await screen.findByRole("region", { name: "What it adds" });
    expect(within(adds).getByText(/2 agents/)).toBeInTheDocument();
    const agents = within(adds)
      .getAllByRole("link")
      .map((link) => link.getAttribute("href"))
      .filter((href) => href?.includes("/agents/r"));
    expect(agents).toEqual([
      `/agents/${encodeURIComponent(node.nodeDid)}/agents/r1`,
      `/agents/${encodeURIComponent(node.nodeDid)}/agents/r2`,
    ]);
    const pages = screen.getByRole("region", { name: "Pages" });
    expect(within(pages).getByRole("link", { name: "Board" })).toHaveAttribute(
      "href",
      "/plugins/demo.board",
    );
    expect(
      screen.getByRole("region", { name: "Interface plugins" }),
    ).toBeInTheDocument();
    const versions = await screen.findByRole("region", { name: "Versions" });
    expect(within(versions).getByText(/Installed/)).toBeInTheDocument();
    expect(within(versions).getByText(/Latest/)).toBeInTheDocument();
  });

  it("turns a pack's plugin off on this device", async () => {
    show(RESEARCH);
    const plugins = await screen.findByRole("region", { name: "Interface plugins" });
    await act(async () => {
      fireEvent.click(within(plugins).getByRole("switch", { name: "Enable Board" }));
    });
    expect(uxDecisions()["demo.board"]).toBe(false);
  });

  it("asks before an install that would grant the pack authority", async () => {
    const installPack = vi
      .fn()
      .mockRejectedValueOnce(new Error("this pack needs --grant-authority"))
      .mockResolvedValueOnce(undefined);
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    show(undefined, { installPack });
    fireEvent.click(await screen.findByRole("tab", { name: "Registry" }));
    const registry = await screen.findByRole("list", { name: "Registry packs" });
    const install = await within(registry).findByRole("button", { name: "Install" });
    await act(async () => {
      fireEvent.click(install);
    });
    await waitFor(() => expect(installPack).toHaveBeenCalledTimes(2));
    expect(confirm).toHaveBeenCalled();
    expect(installPack.mock.calls[0]![0]).toMatchObject({
      package: "acme/docs",
      grantAuthority: false,
    });
    expect(installPack.mock.calls[1]![0]).toMatchObject({ grantAuthority: true });
    confirm.mockRestore();
  });

  it("only browses on a node whose packs are installed on that machine", async () => {
    withFlags({ packs: true });
    const listInstalledPacks = vi.fn(async () => ({ packs: [research] }));
    const app = testApp({
      api: {
        listInstalledPacks,
        searchPacks: async () => ({
          packs: [{ namespace: "acme", name: "docs" }],
          has_more: false,
        }),
      },
      deployments: [{ ...node, nodeDid: "did:key:remote", source: "peer" }],
    });
    renderIn(app, <PacksScreen />);
    expect(screen.getByText(/installed on that machine/)).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Installed packs" })).toBeNull();
    expect(listInstalledPacks).not.toHaveBeenCalled();
    const registry = await screen.findByRole("list", { name: "Registry packs" });
    expect(
      await within(registry).findByRole("button", { name: "Install" }),
    ).toBeDisabled();
  });
});

describe("where Packs is reached", () => {
  it("has a route that reads back what it wrote", () => {
    for (const route of [
      { name: "packs" as const },
      { name: "packs" as const, item: RESEARCH },
    ]) {
      expect(parseRoute(pathFor(route))).toEqual(route);
    }
  });

  it("offers Go to packs and the node's packs in search only while the feature is on", async () => {
    const app = testApp({
      api: { listInstalledPacks: async () => ({ packs: [research] }) },
      snapshot: { bootstrap, client: { deployments: [node] } },
    });
    renderIn(app, <SearchPalette />);
    act(() => openSearch());
    const input = screen.getByRole("combobox", { name: "Search" });
    fireEvent.change(input, { target: { value: "packs" } });
    expect(screen.queryByRole("option", { name: /Go to packs/ })).toBeNull();
    act(() => withFlags({ packs: true }));
    fireEvent.change(input, { target: { value: "go to packs" } });
    expect(
      await screen.findByRole("option", { name: /Go to packs/ }),
    ).toBeInTheDocument();
    fireEvent.change(input, { target: { value: "research" } });
    await waitFor(() =>
      expect(
        screen
          .getAllByRole("option", { name: /research/ })
          .some((o) => o.textContent?.includes("Packs")),
      ).toBe(true),
    );
  });
});
