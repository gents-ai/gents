import { act, fireEvent, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { describe, expect, it, vi } from "vitest";
import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

const { toast } = vi.hoisted(() => ({ toast: vi.fn() }));
vi.mock("sonner", async (actual) => ({
  ...(await actual<typeof import("sonner")>()),
  toast,
}));

import { applyFleetSnapshot } from "../src/hooks/fleetStore";
import { reconcileSelection } from "../src/hooks/selectionReconcile";
import { selection } from "../src/hooks/selectionStore";
import { NodeSwitcher } from "../src/ui/app/NodeSwitcher";
import { preferences } from "../src/ui/preferences";
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

describe("the node the app opens on", () => {
  const launch = (openOn: string | null) => {
    /* no node used last, so a fallback lands on this machine's */
    try {
      localStorage.removeItem("gents.activeNode");
    } catch {}
    preferences.setOpenOn(openOn);
    const stores = shellStores({ selection: { nodeDid: null } });
    applyFleetSnapshot(stores.fleet, {
      bootstrap: { initNodeDid: HOME },
      client: { deployments: fleet() },
    } as unknown as DesktopClientSnapshot);
    reconcileSelection(stores, (nodeDid) =>
      selection.selectNode(stores.selection, nodeDid),
    );
    return stores.selection.getState().nodeDid;
  };

  it("opens on the chosen node over the one used last", () => {
    expect(launch("did:key:vega")).toBe("did:key:vega");
    expect(toast).not.toHaveBeenCalled();
  });

  it("falls back as before when the chosen node isn't listed, and says so once", () => {
    toast.mockClear();
    expect(launch("did:key:gone")).toBe(HOME);
    expect(toast).toHaveBeenCalledTimes(1);
    expect(toast).toHaveBeenCalledWith(
      "The node set to open on isn't listed, so the app opened on Local Agent",
    );
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

  it("waits in the Nodes page's Hidden group, and Show brings it back", async () => {
    preferences.setNodeHidden("did:key:juno", true);
    renderIn(
      testApp({ deployments: fleet(), selection: { nodeDid: HOME } }),
      <AgentsScreen />,
    );
    const group = screen.getByText("Hidden").closest("section")!;
    expect(within(group).getByRole("link", { name: "Juno sessions" })).toBeVisible();
    await userEvent.click(within(group).getByRole("button", { name: "Show" }));
    expect(screen.queryByText("Hidden")).toBeNull();
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
    expect(
      within(screen.getByText("Hidden").closest("section")!).getByRole("link", {
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

  it("is marked Hidden in its node's Agents list, which can show it again", async () => {
    preferences.setAgentHidden(HOME, "ops", true);
    renderIn(
      testApp({ deployments: [node()] }),
      <AgentsPanel deployment={deployment} />,
    );
    expect(screen.getByText("Hidden")).toBeVisible();
    const user = userEvent.setup({ pointerEventsCheck: 0 });
    await user.click(screen.getByRole("button", { name: "More for Ops" }));
    await user.click(await screen.findByRole("menuitem", { name: "Show in menus" }));
    expect(screen.queryByText("Hidden")).toBeNull();
  });
});

describe("the settings' Nodes group", () => {
  it("lists what is hidden, each with Show", async () => {
    preferences.setNodeHidden("did:key:juno", true);
    preferences.setAgentHidden("did:key:vega", "ops", true);
    renderIn(testApp({ deployments: fleet() }), <NodesGroup />);
    expect(
      screen.getByText("Agent on Vega, hidden from the agent picker and search"),
    ).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Show Juno" }));
    await userEvent.click(screen.getByRole("button", { name: "Show Ops" }));
    expect(screen.getByText(/Nothing is hidden/)).toBeVisible();
  });
});
