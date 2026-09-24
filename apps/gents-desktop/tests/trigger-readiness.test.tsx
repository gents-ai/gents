import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { DeploymentView } from "@source-inc/gents-desktop-client";

vi.mock("@/lib/router", () => ({ href: () => "#", navigate: vi.fn() }));

import { triggerReadiness } from "../src/ui/screens/agent/automation";
import { TriggersPanel } from "../src/ui/screens/agent/TriggersPanel";
import type { Shell } from "../src/ui/hooks/useShell";
import { deployment } from "./config-panel-wiring/fixtures";

const withReadiness = (
  behaviors: DeploymentView["behaviorReadiness"]["behaviors"],
): DeploymentView => ({
  ...deployment,
  behaviorReadiness: { ...deployment.behaviorReadiness, behaviors },
});

const trigger = deployment.triggers.find((t) => t.config.trigger_id === "trigger-a")!;

describe("trigger readiness", () => {
  it("defers to the bridge's behavior readiness decision", () => {
    const blocked = withReadiness([
      { state: "unavailable", behaviorId: "default", reason: "credentials_required" },
    ] as DeploymentView["behaviorReadiness"]["behaviors"]);
    expect(triggerReadiness(blocked, trigger)).toEqual({
      ok: false,
      reason: expect.stringContaining("inference credentials are required"),
    });
  });

  it("is not blocked when the bridge says the behavior is ready", () => {
    const ready = withReadiness([
      { state: "ready", behaviorId: "default" },
    ] as DeploymentView["behaviorReadiness"]["behaviors"]);
    expect(triggerReadiness(ready, trigger).ok).toBe(true);
  });

  it("never promises a trigger is Ready", () => {
    const ready = withReadiness([
      { state: "ready", behaviorId: "default" },
    ] as DeploymentView["behaviorReadiness"]["behaviors"]);
    render(
      <TriggersPanel
        shell={{ applyConfig: vi.fn() } as unknown as Shell}
        deployment={ready}
        item="trigger-a"
      />,
    );
    expect(screen.queryByText("Ready")).toBeNull();
    /* nothing to say is said with nothing, not an empty line */
    const header = screen.getByRole("heading", { name: "Trigger A" }).parentElement!;
    for (const line of header.querySelectorAll("p"))
      expect(line.textContent?.trim()).not.toBe("");
  });
});

describe("dependents", () => {
  it("counts subagent targets that point at a behavior", async () => {
    const { dependents } = await import("../src/ui/screens/agent/dependents");
    const withTarget = {
      ...deployment,
      subagentTargets: [
        ...deployment.subagentTargets,
        { behavior_id: "ops" } as DeploymentView["subagentTargets"][number],
      ],
    };
    expect(dependents(withTarget, "behavior", "ops")).toContain("1 subagent target");
  });
});
