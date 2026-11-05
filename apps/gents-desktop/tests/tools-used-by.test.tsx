import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ToolsUsedBy } from "../src/ui/screens/agent/ToolsSummary";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const tools = (id: string) => deployment.tools.find((t) => t.tools_id === id)!;

describe("a tool set's side sheet", () => {
  it("names the agents that use it, through their contexts, each a link", () => {
    renderIn(
      testApp(),
      <ToolsUsedBy deployment={deployment} tools={tools("tools-a")} />,
    );
    const agent = screen.getByRole("link", { name: "Default" });
    expect(agent.getAttribute("href")).toContain("/agents/");
    expect(screen.getByText("Used by")).toBeInTheDocument();
  });

  it("says so when no agent uses it", () => {
    const unused = { ...tools("tools-a"), tools_id: "tools-unused" };
    renderIn(testApp(), <ToolsUsedBy deployment={deployment} tools={unused} />);
    expect(screen.getByText("Not used by any agent")).toBeInTheDocument();
  });
});
