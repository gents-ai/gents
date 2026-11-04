import { screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { AgentCard } from "../src/ui/screens/agent/AgentCard";
import { renderIn, testApp } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

function card(node: DeploymentView = deployment) {
  const app = testApp({
    api: {
      fetchOperationsSnapshot: vi.fn().mockResolvedValue({ backgroundedTools: [] }),
    },
    snapshot: { bootstrap, client: { deployments: [node] } },
  });
  renderIn(app, <AgentCard deployment={node} avatar={null} onAvatar={() => {}} />);
}

const summary = (label: string) =>
  screen.getByText(label, { selector: "span" }).closest("a")!;

const trigger = deployment.triggers[0]!;
const withTrigger = (fields: Partial<typeof trigger>): DeploymentView => ({
  ...deployment,
  triggers: [{ ...trigger, ...fields }],
});

describe("the node page's summary cards", () => {
  it("counts the node's agents beside their marks, and leads to them", () => {
    card();
    const agents = summary("Agents");
    expect(agents).toHaveTextContent(`${deployment.agents.length} agents`);
    expect(agents.getAttribute("href")).toContain("agents");
  });

  it("marks four agents, then counts the rest as +n", () => {
    const agent = deployment.agents[0]!;
    const agents = Array.from({ length: 6 }, (_, i) => ({
      ...agent,
      agentId: `agent-${i}`,
      displayName: `Agent ${i}`,
    }));
    card({ ...deployment, agents });
    const card_ = summary("Agents");
    expect(card_).toHaveTextContent("+2");
    expect(card_).toHaveTextContent("6 agents");
  });

  it("says when the next automation runs", () => {
    const soon = new Date(Date.now() + 2 * 3_600_000).toISOString();
    card(withTrigger({ nextRunAt: soon }));
    const automations = summary("Automations");
    expect(automations).toHaveTextContent(
      `${deployment.tasks.length} automations · next in 2h`,
    );
    expect(automations.getAttribute("href")).toContain("tasks");
  });

  it("counts automations whose last run failed, as the runtime records it", () => {
    card(withTrigger({ lastStatus: "error", lastError: "provider refused" }));
    expect(summary("Automations")).toHaveTextContent("· 1 failed");
  });

  it("leaves an automation that is off out of the failed count", () => {
    card(
      withTrigger({
        lastStatus: "error",
        config: { ...trigger.config, enabled: false },
      }),
    );
    expect(summary("Automations")).not.toHaveTextContent("failed");
  });
});
