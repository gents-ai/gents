import { act, fireEvent, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

const { toast } = vi.hoisted(() => ({ toast: vi.fn() }));
vi.mock("sonner", async (actual) => ({
  ...(await actual<typeof import("sonner")>()),
  toast,
}));

import { applyFleetSnapshot } from "../src/hooks/fleetStore";
import { createDraftStore, writeDraft } from "../src/hooks/draftStore";
import { OPEN_ON_WAIT_MS, reconcileSelection } from "../src/hooks/selectionReconcile";
import { selection } from "../src/hooks/selectionStore";
import { NodeSwitcher } from "../src/ui/app/NodeSwitcher";
import { bindNav } from "../src/ui/lib/router";
import type { Nav } from "@gents/shell";
import { openOnNode, preferences } from "../src/ui/preferences";
import { AgentsScreen } from "../src/ui/screens/AgentsScreen";
import { AgentPicker } from "../src/ui/screens/BehaviorPicker";
import { openSearch, SearchPalette } from "../src/ui/screens/SearchPalette";
import { AgentsPanel } from "../src/ui/screens/agent/BehaviorsPanel";
import { NodesGroup } from "../src/ui/screens/settings/NodesGroup";
import { node, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";
import { shellStores } from "./shell-fixture";

const HOME = deployment.nodeDid;
const remote = (nodeDid: string, name: string) =>
  node({
    nodeDid,
    label: name,
    source: "enrollment",
    node: { ...deployment.node, nodeDid, displayName: name },
    agents: deployment.agents.map((a) => ({ ...a, nodeDid })),
  });
const fleet = () => [
  node(),
  remote("did:key:juno", "Juno"),
  remote("did:key:vega", "Vega"),
];

describe("the startup node", () => {
  afterEach(() => {
    vi.useRealTimers();
    toast.mockClear();
  });

  const snapshot = (deployments: ReturnType<typeof node>[]) =>
    ({
      bootstrap: { initNodeDid: HOME },
      client: { deployments },
    }) as unknown as DesktopClientSnapshot;
  const launch = (openOn: string | null, deployments = fleet()) => {
    /* no node used last, so a fallback lands on this machine's */
    try {
      localStorage.removeItem("gents.activeNode");
    } catch {}
    preferences.setOpenOn(openOn);
    const stores = shellStores({ selection: { nodeDid: null } });
    const drafts = createDraftStore();
    applyFleetSnapshot(stores.fleet, snapshot(deployments));
    reconcileSelection(
      stores,
      (nodeDid) => selection.selectNode(stores.selection, nodeDid),
      drafts,
    );
    return {
      stores,
      drafts,
      at: () => stores.selection.getState().nodeDid,
      arrive: (next: ReturnType<typeof node>[]) =>
        act(() => applyFleetSnapshot(stores.fleet, snapshot(next))),
    };
  };
  const unreached = (lastError: string | null) => ({
    ...remote("did:key:vega", "Vega"),
    dialSucceeded: false,
    lastError,
  });
  const withoutVega = () => fleet().filter((n) => n.nodeDid !== "did:key:vega");

  it("opens on the chosen node over the one used last", () => {
    expect(launch("did:key:vega").at()).toBe("did:key:vega");
    expect(toast).not.toHaveBeenCalled();
  });

  it("falls back from a chosen node that can't be reached, and says so", () => {
    const app = launch("did:key:vega", [...withoutVega(), unreached("dial failed")]);
    expect(app.at()).toBe(HOME);
    expect(toast).toHaveBeenCalledTimes(1);
    expect(toast).toHaveBeenCalledWith(
      "Vega isn't reachable, so the app opened on Local Agent",
    );
  });

  it("opens on a chosen node that arrives just after launch, quietly", () => {
    vi.useFakeTimers();
    const app = launch("did:key:vega", withoutVega());
    expect(app.at()).toBe(HOME);
    vi.advanceTimersByTime(2000);
    app.arrive(fleet());
    expect(app.at()).toBe("did:key:vega");
    vi.advanceTimersByTime(OPEN_ON_WAIT_MS);
    expect(toast).not.toHaveBeenCalled();
  });

  it("opens on a chosen node once its first dial succeeds", () => {
    vi.useFakeTimers();
    const app = launch("did:key:vega", [...withoutVega(), unreached(null)]);
    expect(app.at()).toBe(HOME);
    app.arrive(fleet());
    expect(app.at()).toBe("did:key:vega");
    expect(toast).not.toHaveBeenCalled();
  });

  it("stays where the person has begun working: a draft, or a route", () => {
    vi.useFakeTimers();
    const typed = launch("did:key:vega", withoutVega());
    writeDraft(typed.drafts, "new", "hello");
    typed.arrive(fleet());
    expect(typed.at()).toBe(HOME);

    const moved = launch("did:key:vega", withoutVega());
    bindNav({ route: { name: "sessions" } } as unknown as Nav);
    bindNav({ route: { name: "mailbox" } } as unknown as Nav);
    bindNav(null);
    moved.arrive(fleet());
    expect(moved.at()).toBe(HOME);
    expect(toast).not.toHaveBeenCalled();
  });

  it("says once, after a while, that a chosen node never came", () => {
    vi.useFakeTimers();
    const app = launch("did:key:gone");
    expect(app.at()).toBe(HOME);
    expect(toast).not.toHaveBeenCalled();
    vi.advanceTimersByTime(OPEN_ON_WAIT_MS);
    expect(toast).toHaveBeenCalledTimes(1);
    expect(toast).toHaveBeenCalledWith(
      "The startup node isn't listed, so the app opened on Local Agent",
    );
    app.arrive(fleet());
    expect(app.at()).toBe(HOME);
  });
});

describe("a hidden node", () => {
  it("is left out of the node switcher, unless it's the node in use", async () => {
    preferences.setNodeHidden("did:key:juno", true);
    preferences.setNodeHidden(HOME, true);
    renderIn(
      testApp({ deployments: fleet(), selection: { nodeDid: HOME } }),
      <TooltipProvider>
        <NodeSwitcher route={{ name: "mailbox" }} current={HOME} />
      </TooltipProvider>,
    );
    await userEvent
      .setup({ pointerEventsCheck: 0 })
      .click(screen.getByRole("button", { name: "Switch node" }));
    const names = (await screen.findAllByRole("menuitemradio")).map(
      (i) => i.textContent,
    );
    expect(names).toEqual([
      expect.stringContaining("Local Agent"),
      expect.stringContaining("Vega"),
    ]);
  });

  it("is left out of search, with its agents, as a hidden agent is", () => {
    preferences.setNodeHidden("did:key:juno", true);
    preferences.setAgentHidden("did:key:vega", "ops", true);
    renderIn(
      testApp({ deployments: fleet(), selection: { nodeDid: HOME } }),
      <SearchPalette />,
    );
    act(() => openSearch());
    const field = screen.getByRole("combobox", { name: "Search" });
    fireEvent.change(field, { target: { value: "juno" } });
    expect(screen.queryByRole("group", { name: "Nodes" })).toBeNull();
    fireEvent.change(field, { target: { value: "ops" } });
    const agents = within(screen.getByRole("group", { name: "Agents" }))
      .getAllByRole("option")
      .map((o) => o.textContent);
    /* Ops on this machine's node only: Juno is hidden, and Vega's Ops is */
    expect(agents).toEqual([expect.stringContaining("Local Agent")]);
  });

  it("waits folded in the Nodes page's Hidden group, which stays open once opened, and Show brings it back", async () => {
    localStorage.removeItem("gents.config.packGroups.nodes");
    preferences.setNodeHidden("did:key:juno", true);
    const app = testApp({ deployments: fleet(), selection: { nodeDid: HOME } });
    const { unmount } = renderIn(app, <AgentsScreen />);
    const header = screen.getByRole("button", { name: /Hidden/ });
    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("link", { name: "Juno sessions" })).toBeNull();
    await userEvent.click(header);
    unmount();

    /* opened, it stays open on this device */
    renderIn(app, <AgentsScreen />);
    expect(screen.getByRole("button", { name: /Hidden/ })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    const group = screen.getByRole("button", { name: /Hidden/ }).closest("section")!;
    await userEvent.click(within(group).getByRole("button", { name: "Show" }));
    expect(screen.queryByRole("button", { name: /Hidden/ })).toBeNull();
    const remoteNodes = screen.getByText("Remote nodes").closest("section")!;
    expect(
      within(remoteNodes).getByRole("link", { name: "Juno sessions" }),
    ).toBeVisible();
  });

  it("asks before hiding the node in use", async () => {
    const user = userEvent.setup({ pointerEventsCheck: 0 });
    renderIn(
      testApp({ deployments: fleet(), selection: { nodeDid: HOME } }),
      <AgentsScreen />,
    );
    await user.click(screen.getByRole("button", { name: "Local Agent actions" }));
    await user.click(await screen.findByRole("menuitem", { name: "Hide from menus" }));
    const ask = await screen.findByRole("alertdialog");
    expect(ask).toHaveTextContent("Hide Local Agent?");
    await user.click(within(ask).getByRole("button", { name: "Hide" }));
    const header = screen.getByRole("button", { name: /Hidden/ });
    if (header.getAttribute("aria-expanded") === "false") await user.click(header);
    expect(
      within(header.closest("section")!).getByRole("link", {
        name: "Local Agent sessions",
      }),
    ).toBeVisible();
  });
});

describe("a hidden agent", () => {
  it("is left out of the agent picker, unless it's the one chosen", async () => {
    preferences.setAgentHidden(HOME, "ops", true);
    const app = testApp({ deployments: [node()] });
    const { rerender } = renderIn(
      app,
      <AgentPicker deployment={deployment} agentId="default" onChange={vi.fn()} />,
    );
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Agent" }));
    const names = () =>
      within(screen.getByRole("listbox", { name: "Agent" }))
        .getAllByRole("option")
        .map((o) => o.textContent);
    expect(names()).toEqual([expect.stringContaining("Default")]);
    await user.keyboard("{Escape}");
    rerender(<AgentPicker deployment={deployment} agentId="ops" onChange={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Agent" }));
    expect(names()).toEqual(expect.arrayContaining([expect.stringContaining("Ops")]));
  });

  it("waits folded under Hidden at the foot of its node's Agents list, with Show", async () => {
    localStorage.removeItem("gents.config.packGroups.agents.hidden");
    preferences.setAgentHidden(HOME, "ops", true);
    renderIn(
      testApp({ deployments: [node()] }),
      <AgentsPanel deployment={deployment} />,
    );
    const header = screen.getByRole("button", { name: /Hidden/ });
    expect(header).toHaveTextContent("Hidden· 1");
    expect(header).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("button", { name: "Show Ops" })).toBeNull();
    await userEvent.click(header);
    await userEvent.click(screen.getByRole("button", { name: "Show Ops" }));
    expect(screen.queryByRole("button", { name: /Hidden/ })).toBeNull();
    expect(screen.getByRole("button", { name: "More for Ops" })).toBeVisible();
  });
});

describe("the settings' Nodes group", () => {
  it("offers Last used, then this machine's node, then the remote ones", async () => {
    renderIn(testApp({ deployments: fleet() }), <NodesGroup />);
    await userEvent.click(screen.getByRole("combobox", { name: "Startup node" }));
    const listbox = await screen.findByRole("listbox");
    expect(
      within(listbox)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Last used", "Local Agent", "Juno", "Vega"]);
    expect(within(listbox).getByRole("group", { name: "Local" })).toHaveTextContent(
      "Local Agent",
    );
    expect(within(listbox).getByRole("group", { name: "Remote" })).toHaveTextContent(
      "JunoVega",
    );
  });

  it("still names a startup node that is no longer listed", async () => {
    preferences.setOpenOn("did:key:gone");
    renderIn(testApp({ deployments: fleet() }), <NodesGroup />);
    const field = screen.getByRole("combobox", { name: "Startup node" });
    expect(field).toHaveTextContent("Node …key:gone (not listed)");
    await userEvent.click(field);
    expect(
      await screen.findByRole("option", { name: "Node …key:gone (not listed)" }),
    ).toBeVisible();
  });

  it("finds a startup node by name, dropping an empty group and keeping Last used", async () => {
    renderIn(testApp({ deployments: fleet() }), <NodesGroup />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox", { name: "Startup node" }));
    await user.type(
      await screen.findByRole("combobox", { name: "Search nodes" }),
      "ju",
    );
    const listbox = screen.getByRole("listbox");
    expect(
      within(listbox)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Last used", "Juno"]);
    expect(within(listbox).queryByRole("group", { name: "Local" })).toBeNull();
    await user.click(within(listbox).getByRole("option", { name: "Juno" }));
    expect(openOnNode()).toBe("did:key:juno");
    expect(screen.getByRole("combobox", { name: "Startup node" })).toHaveTextContent(
      "Juno",
    );

    /* Last used clears the choice */
    await user.click(screen.getByRole("combobox", { name: "Startup node" }));
    await user.click(await screen.findByRole("option", { name: "Last used" }));
    expect(openOnNode()).toBeNull();
    expect(screen.getByRole("combobox", { name: "Startup node" })).toHaveTextContent(
      "Last used",
    );
  });

  it("keeps one Hidden row, listing what is hidden inside it, each with Show", async () => {
    renderIn(testApp({ deployments: fleet() }), <NodesGroup />);
    expect(screen.getByText("Hidden")).toBeVisible();
    expect(screen.getByText(/Nothing is hidden/)).toBeVisible();
    expect(screen.queryByRole("list", { name: "Hidden" })).toBeNull();

    act(() => {
      preferences.setNodeHidden("did:key:juno", true);
      preferences.setAgentHidden("did:key:vega", "ops", true);
    });
    /* the same row, its items nested in it rather than rows of their own */
    expect(screen.getByText("Hidden")).toBeVisible();
    expect(
      screen.getByText(
        "Left out of the node switcher, the sidebar and search on this device.",
      ),
    ).toBeVisible();
    const list = screen.getByRole("list", { name: "Hidden" });
    expect(
      within(list)
        .getAllByRole("listitem")
        .map((i) => i.textContent),
    ).toEqual(["JunoNodeShow", "OpsAgent on VegaShow"]);
    await userEvent.click(within(list).getByRole("button", { name: "Show Juno" }));
    await userEvent.click(within(list).getByRole("button", { name: "Show Ops" }));
    expect(screen.getByText(/Nothing is hidden/)).toBeVisible();
  });
});
