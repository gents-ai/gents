import { screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import type { NodeView } from "../src/hooks/fleetStore";
import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { AgentsScreen } from "../src/ui/screens/AgentsScreen";
import { deployment } from "./config-panel-wiring/fixtures";
import { node, renderIn, testApp } from "./app-fixture";

const remote = "did:key:remote";

function withRemote(overrides: Partial<NodeView>) {
  return testApp({
    api: { fetchOperationsSnapshot: vi.fn().mockResolvedValue(null) },
    deployments: [
      node(),
      node({
        nodeDid: remote,
        source: "enrollment",
        label: "studio-1",
        ...overrides,
        node: { ...deployment.node, nodeDid: remote, ...overrides.node },
      }),
    ],
    selection: { nodeDid: remote },
  });
}

/* a fact's row, found by its label */
const factOf = (label: string) =>
  screen.getByText(label).closest("[data-slot=field-row]") as HTMLElement;

function remotePage(overrides: Partial<NodeView>) {
  renderIn(
    withRemote(overrides),
    <TooltipProvider>
      <AgentScreen nodeDid={remote} section="agent" />
    </TooltipProvider>,
  );
}

describe("a remote node's own details", () => {
  it("shows what the node shares, read only", () => {
    remotePage({
      node: { ...deployment.node, defaultAgentDid: "ops", displayName: "Amy" },
      nodeConfig: { node_did: remote, max_request_hop: 3 },
    });
    expect(screen.getByRole("status")).toHaveTextContent(/^Read-only here/);
    const agent = within(factOf("Default agent")).getByRole("link", { name: "Ops" });
    expect(agent.getAttribute("href")).toContain("ops");
    expect(within(factOf("Maximum request hop")).getByText("3")).toBeVisible();
    /* nothing that would change them */
    expect(screen.queryByRole("combobox", { name: /Default agent/ })).toBeNull();
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  });

  it("says no limit when the node shares settings without one", () => {
    remotePage({ nodeConfig: { node_did: remote } });
    expect(within(factOf("Maximum request hop")).getByText("No limit")).toBeVisible();
  });

  it("says what a node on an older build doesn't share, rather than looking unset", () => {
    remotePage({
      node: { ...deployment.node, defaultAgentDid: null, displayName: null },
      nodeConfig: null,
    });
    expect(
      within(factOf("Default agent")).getByText("Not shared by this node"),
    ).toBeVisible();
    expect(
      within(factOf("Maximum request hop")).getByText("Not shared by this node"),
    ).toBeVisible();
  });
});

describe("the Nodes page", () => {
  it("marks a node that is turned off", () => {
    renderIn(
      withRemote({ node: { ...deployment.node, enabled: false, displayName: "Amy" } }),
      <TooltipProvider>
        <AgentsScreen />
      </TooltipProvider>,
    );
    const row = screen.getByRole("link", { name: "Amy sessions" }).closest("li")!;
    expect(within(row).getByText("Disabled")).toBeVisible();
  });
});
