/* The two shell consumers re-attached in the merge with main: a row a UX
   plugin contributes to the `nav` area shows in the rail and the panel
   beside the app's own rows, and a section it contributes to
   `agent.sections` is a tab on the configuration screen that draws the
   plugin's page behind a boundary. */
import { act, screen } from "@testing-library/react";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { afterEach, describe, expect, it, vi } from "vitest";

import { NavPanel } from "@/app/RailFlyout";
import { Rail } from "@/app/Rail";
import { registry } from "@/contrib/registry";
import { AGENT_SECTIONS_AREA, NAV_AREA, type AgentSectionData } from "@/contrib/types";
import type { NavItem } from "@/app/navRegistry";
import { AgentScreen } from "@/screens/agent/AgentScreen";
import { node, renderIn, testApp } from "./app-fixture";

const disposers: Array<() => void> = [];
afterEach(() => {
  disposers.splice(0).forEach((dispose) => dispose());
  vi.restoreAllMocks();
});

const boardRow: NavItem = {
  id: "board:nav",
  label: "Board",
  icon: <span data-testid="board-icon">◫</span>,
  to: { name: "agent", agentDid: "did:key:a", section: "board:page" },
  active: (r) => r.name === "agent" && r.section === "board:page",
  placement: "footer",
};

describe("contributed nav rows", () => {
  it("show on the rail and in the panel after the app's own rows", () => {
    disposers.push(
      registry.register({
        id: "board:nav",
        area: NAV_AREA,
        source: "plugin:board",
        data: boardRow,
      }),
    );
    const app = testApp({ deployments: [node({ agentDid: "did:key:a" })] });
    renderIn(
      app,
      <TooltipProvider>
        <Rail route={{ name: "sessions" }} settings={null} />
        <NavPanel route={{ name: "sessions" }} settings={null} syncRow={false} />
      </TooltipProvider>,
    );
    /* one on the rail (its aria-label), one in the panel (its text, which
       the icon precedes), both linking to the page */
    const links = screen.getAllByRole("link", { name: /Board/ });
    expect(links).toHaveLength(2);
    expect(links.every((a) => a.getAttribute("href")?.includes("board:page"))).toBe(
      true,
    );
    /* the app's own rows are still there and still come first in the panel */
    const panelLabels = screen
      .getAllByRole("link")
      .map((a) => a.textContent?.replace(/^◫/, "").trim())
      .filter((t) => t === "Sessions" || t === "Board" || t?.startsWith("Nodes"));
    expect(panelLabels.indexOf("Sessions")).toBeLessThan(panelLabels.indexOf("Board"));
    expect(panelLabels.indexOf("Board")).toBeLessThan(
      panelLabels.findIndex((t) => t?.startsWith("Nodes")),
    );
  });

  it("light for the route the row claims", () => {
    disposers.push(
      registry.register({
        id: "board:nav",
        area: NAV_AREA,
        source: "plugin:board",
        data: boardRow,
      }),
    );
    const app = testApp({ deployments: [node({ agentDid: "did:key:a" })] });
    renderIn(
      app,
      <TooltipProvider>
        <Rail
          route={{ name: "agent", agentDid: "did:key:a", section: "board:page" }}
          settings={null}
        />
      </TooltipProvider>,
    );
    expect(screen.getByRole("link", { name: "Board" })).toHaveAttribute(
      "aria-current",
      "page",
    );
  });
});

describe("contributed agent sections", () => {
  const section: AgentSectionData = {
    group: "Packs",
    label: "Board",
    render: ({ agentDid, item }) => (
      <p data-testid="board-page">
        board for {agentDid}
        {item ? ` #${item}` : ""}
      </p>
    ),
  };

  it("list as a tab in their group and draw the plugin's page for the route", () => {
    disposers.push(
      registry.register({
        id: "board:page",
        area: AGENT_SECTIONS_AREA,
        source: "plugin:board",
        data: section,
      }),
    );
    const app = testApp({ deployments: [node({ agentDid: "did:key:a" })] });
    renderIn(
      app,
      <TooltipProvider>
        <AgentScreen agentDid="did:key:a" section="board:page" item="7" />
      </TooltipProvider>,
    );
    expect(screen.getByTestId("board-page")).toHaveTextContent(
      "board for did:key:a #7",
    );
    expect(screen.getByTestId("agent-screen")).toHaveAttribute(
      "data-section",
      "board:page",
    );
    /* the tab sits in the Packs group beside the app's own */
    const tab = screen.getByRole("link", { name: /Board/ });
    expect(tab).toHaveAttribute("aria-current", "page");
  });

  it("contain a page that throws without taking the screen down", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    disposers.push(
      registry.register({
        id: "boom:page",
        area: AGENT_SECTIONS_AREA,
        source: "plugin:boom",
        data: {
          ...section,
          label: "Boom",
          render: () => {
            throw new Error("page failed");
          },
        },
      }),
    );
    const app = testApp({ deployments: [node({ agentDid: "did:key:a" })] });
    renderIn(
      app,
      <TooltipProvider>
        <AgentScreen agentDid="did:key:a" section="boom:page" />
      </TooltipProvider>,
    );
    expect(screen.getByTestId("contrib-error-pane")).toHaveTextContent("page failed");
    expect(screen.getByTestId("agent-screen")).toBeInTheDocument();
  });

  it("vanish from the sidebar when the plugin unregisters", () => {
    const dispose = registry.register({
      id: "board:page",
      area: AGENT_SECTIONS_AREA,
      source: "plugin:board",
      data: section,
    });
    const app = testApp({ deployments: [node({ agentDid: "did:key:a" })] });
    renderIn(
      app,
      <TooltipProvider>
        <AgentScreen agentDid="did:key:a" section="packs" />
      </TooltipProvider>,
    );
    expect(screen.getByRole("link", { name: /Board/ })).toBeInTheDocument();
    act(() => dispose());
    expect(screen.queryByRole("link", { name: /Board/ })).toBeNull();
  });
});
