import { screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { renderIn, testApp } from "./app-fixture";
import type { DeploymentView } from "@source-inc/gents-desktop-client";

vi.mock("@/lib/router", () => ({ href: () => "#", navigate: vi.fn() }));

import { triggerReadiness } from "../src/ui/screens/agent/automation";
import { TriggersPanel } from "../src/ui/screens/agent/TriggersPanel";
import { deployment } from "./config-panel-wiring/fixtures";

const withReadiness = (
  agents: DeploymentView["nodeReadiness"]["agents"],
): DeploymentView => ({
  ...deployment,
  nodeReadiness: { ...deployment.nodeReadiness, agents },
});

const trigger = deployment.triggers.find((t) => t.config.trigger_id === "trigger-a")!;

describe("trigger readiness", () => {
  it("defers to the bridge's agent readiness decision", () => {
    const blocked = withReadiness([
      { state: "unavailable", agentId: "default", reason: "credentials_required" },
    ] as DeploymentView["nodeReadiness"]["agents"]);
    expect(triggerReadiness(blocked, trigger)).toEqual({
      ok: false,
      reason: expect.stringContaining("inference credentials are required"),
    });
  });

  it("is not blocked when the bridge says the agent is ready", () => {
    const ready = withReadiness([
      { state: "ready", agentId: "default" },
    ] as DeploymentView["nodeReadiness"]["agents"]);
    expect(triggerReadiness(ready, trigger).ok).toBe(true);
  });

  it("never promises a trigger is Ready", () => {
    const ready = withReadiness([
      { state: "ready", agentId: "default" },
    ] as DeploymentView["nodeReadiness"]["agents"]);
    renderIn(testApp(), <TriggersPanel deployment={ready} item="trigger-a" />);
    expect(screen.queryByText("Ready")).toBeNull();
    /* nothing to say is said with nothing, not an empty line */
    const header = screen.getByRole("heading", { name: "Trigger A" }).parentElement!;
    for (const line of header.querySelectorAll("p"))
      expect(line.textContent?.trim()).not.toBe("");
  });
});

describe("dependents", () => {
  it("counts agent targets that point at an agent", async () => {
    const { dependents } = await import("../src/ui/screens/agent/dependents");
    const withTarget = {
      ...deployment,
      agentTargets: [
        ...deployment.agentTargets,
        { agent_id: "ops" } as DeploymentView["agentTargets"][number],
      ],
    };
    expect(dependents(withTarget, "agent", "ops")).toContain("1 agent target");
  });
});
