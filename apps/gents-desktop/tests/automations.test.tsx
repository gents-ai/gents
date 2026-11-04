import { act, fireEvent, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import {
  automationsOf,
  inTab,
  runsOf,
  type Automation,
} from "../src/ui/screens/automations/model";
import { AutomationsScreen } from "../src/ui/screens/AutomationsScreen";
import { renderIn, renderRouted, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const NOW = Date.parse("2026-11-04T12:00:00Z");
const trigger = deployment.triggers.find((t) => t.config.trigger_id === "trigger-a")!;
const withTrigger = (fields: Partial<typeof trigger>): DeploymentView => ({
  ...deployment,
  triggers: [{ ...trigger, ...fields }],
});
const byId = (list: Automation[], id: string) => list.find((a) => a.id === id)!;

describe("the node's automations", () => {
  it("has one per trigger, and a run-by-hand one per task no trigger starts", () => {
    const list = automationsOf(deployment);
    expect(list.map((a) => [a.id, a.kind])).toEqual([
      ["trigger-a", "schedule"],
      ["task:task-b", "manual"],
    ]);
    expect(byId(list, "task:task-b").when).toBe("Run by hand");
  });

  it("puts an automation under Next up, Needs attention or Off by its state", () => {
    const soon = new Date(NOW + 3_600_000).toISOString();
    const next = byId(automationsOf(withTrigger({ nextRunAt: soon })), "trigger-a");
    expect(inTab(next, "next", NOW)).toBe(true);
    const failed = byId(
      automationsOf(withTrigger({ lastStatus: "error" })),
      "trigger-a",
    );
    expect(inTab(failed, "attention", NOW)).toBe(true);
    const off = byId(
      automationsOf(withTrigger({ config: { ...trigger.config, enabled: false } })),
      "trigger-a",
    );
    expect(inTab(off, "off", NOW)).toBe(true);
    expect(inTab(off, "attention", NOW)).toBe(false);
  });

  it("lists a trigger's own runs, newest first", () => {
    const run = (
      requestId: string,
      createdAt: string,
      causedByTriggerId: string | null,
    ) => ({
      requestId,
      requestDocId: null,
      nodeDid: deployment.nodeDid,
      requesterDid: null,
      sessionId: `s-${requestId}`,
      agentId: "default",
      lifecycleState: "completed",
      executionOrigin: null,
      causedByTriggerId,
      causedByTriggerKind: causedByTriggerId ? "schedule" : null,
      createdAt,
    });
    const node: DeploymentView = {
      ...deployment,
      tasks: deployment.tasks.map((t) =>
        t.taskId === "task-a"
          ? {
              ...t,
              runHistory: [
                run("r1", "2026-11-04T09:00:00Z", "trigger-a"),
                run("r2", "2026-11-04T10:00:00Z", null),
                run("r3", "2026-11-04T11:00:00Z", "trigger-a"),
              ],
            }
          : t,
      ),
    } as DeploymentView;
    expect(
      runsOf(byId(automationsOf(node), "trigger-a")).map((r) => r.requestId),
    ).toEqual(["r3", "r1"]);
  });
});

describe("the Automations page", () => {
  const page = (item?: string) => {
    withFlags({ automations: true });
    const changeConfig = vi.fn().mockResolvedValue({});
    const runTask = vi.fn().mockResolvedValue({ sessionId: "s-new" });
    const app = testApp({
      api: { patchConfigComponents: changeConfig, runTask },
      snapshot: { bootstrap, client: { deployments: [deployment] } },
    });
    renderIn(app, <AutomationsScreen item={item} />);
    return { changeConfig, runTask, app };
  };

  it("lists the active node's automations, the hand-run ones under their own heading", () => {
    page();
    expect(screen.getByRole("link", { name: /Trigger A/ })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Run by hand" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /Task B/ })).toBeInTheDocument();
  });

  it("opens an idea as a new automation in place, filled in, and creates it", async () => {
    withFlags({ automations: true });
    const changeConfig = vi.fn().mockResolvedValue({});
    const app = testApp({
      api: { patchConfigComponents: changeConfig, applyConfigComponents: changeConfig },
      snapshot: { bootstrap, client: { deployments: [deployment] } },
    });
    renderRouted(app, { name: "automations" }, (route) =>
      route.name === "automations" ? <AutomationsScreen item={route.item} /> : null,
    );
    await act(async () => {
      fireEvent.click(
        within(screen.getByLabelText("Ideas")).getByRole("button", {
          name: /Morning summary/,
        }),
      );
    });
    expect(screen.getByLabelText("Name")).toHaveValue("Morning summary");
    expect(
      (screen.getByLabelText("What the agent should do") as HTMLTextAreaElement).value,
    ).toContain("Summarize what happened");
    expect(
      screen.getByRole("button", { name: "When: Every weekday at 09:00" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Create" }));
    });
    expect(changeConfig).toHaveBeenCalled();
    expect(screen.queryByLabelText("What the agent should do")).not.toBeInTheDocument();
  });

  it("turns an open automation off", async () => {
    const { changeConfig } = page("trigger-a");
    await act(async () => {
      fireEvent.click(screen.getByRole("switch", { name: "Trigger A is on" }));
    });
    expect(changeConfig).toHaveBeenCalledWith(
      expect.objectContaining({
        patches: [
          { collection: "Trigger", id: "trigger-a", changes: { enabled: false } },
        ],
      }),
    );
  });

  it("duplicates an open hand-run automation from its menu", async () => {
    const saveTaskConfig = vi.fn().mockResolvedValue({});
    withFlags({ automations: true });
    const app = testApp({
      api: { saveTaskConfig },
      snapshot: { bootstrap, client: { deployments: [deployment] } },
    });
    renderIn(app, <AutomationsScreen item="task:task-b" />);
    fireEvent.click(screen.getByRole("button", { name: "More for Task B" }));
    await act(async () => {
      fireEvent.click(await screen.findByRole("menuitem", { name: "Duplicate" }));
    });
    expect(saveTaskConfig).toHaveBeenCalledWith(
      expect.objectContaining({
        document: expect.objectContaining({ display_name: "Task B copy" }),
      }),
    );
  });

  it("shows an open one beside the list, and runs it now", async () => {
    const { runTask } = page("trigger-a");
    const run = await screen.findByRole("button", { name: /Run now/ });
    await act(async () => {
      fireEvent.click(run);
    });
    expect(runTask).toHaveBeenCalledWith(
      expect.objectContaining({ taskId: "task-a", nodeDid: deployment.nodeDid }),
    );
    expect(screen.getByText("No runs recorded yet.")).toBeInTheDocument();
  });
});

describe("the page's route and command", () => {
  it("round-trips its path, with and without an open automation", async () => {
    const { parseRoute, pathFor } = await import("../src/ui/lib/router");
    expect(parseRoute(pathFor({ name: "automations" }))).toEqual({
      name: "automations",
    });
    expect(parseRoute(pathFor({ name: "automations", item: "trigger a" }))).toEqual({
      name: "automations",
      item: "trigger a",
    });
  });

  it("offers Go to automations only while the feature is on", async () => {
    await import("../src/ui/commands/app");
    const { getCommand } = await import("../src/ui/commands/commands");
    const go = getCommand("page.automations")!;
    withFlags({ automations: false });
    expect(go.when!()).toBe(false);
    withFlags({ automations: true });
    expect(go.when!()).toBe(true);
  });
});
