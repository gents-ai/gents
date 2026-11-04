import { act, fireEvent, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import {
  automationsOf,
  byPack,
  inTab,
  runsOf,
  type Automation,
} from "../src/ui/screens/automations/model";
import { AutomationsScreen } from "../src/ui/screens/AutomationsScreen";
import {
  OTHER,
  collectionGroups,
  collectionInWords,
} from "../src/ui/screens/agent/CollectionPicker";
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

describe("what an event automation can watch", () => {
  it("names collections in words", () => {
    expect(collectionInWords("MailboxItem")).toBe("Mailbox items");
    expect(collectionInWords("acme_doc_comment")).toBe("Acme doc comments");
  });

  it("offers the safe core collections, the packs' own, those already watched, and Other", () => {
    const node: DeploymentView = {
      ...deployment,
      datastoreToolSurfaces: [
        {
          surface_id: "s",
          node_did: deployment.nodeDid,
          entries: [{ kind: "query", tool_name: "find_notes", collection: "AcmeNote" }],
        },
      ],
      eventSources: [
        {
          node_did: deployment.nodeDid,
          event_source_id: "e",
          source_collection: "GameMove",
        },
      ],
    } as DeploymentView;
    expect(collectionGroups(node)).toEqual([
      { value: "Gents", items: ["MailboxItem"] },
      { value: "From your packs", items: ["AcmeNote"] },
      { value: "Already watched", items: ["GameMove"] },
      { value: "", items: [OTHER] },
    ]);
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

  it("lists the active node's automations together, the hand-run ones last", () => {
    page();
    const links = screen
      .getAllByRole("link")
      .map((l) => l.textContent ?? "")
      .filter((t) => /Trigger A|Task B/.test(t));
    expect(links).toHaveLength(2);
    expect(links[0]).toMatch(/Trigger A/);
    expect(links[1]).toMatch(/Task B.*Run by hand/);
    expect(
      screen.queryByRole("heading", { name: "Run by hand" }),
    ).not.toBeInTheDocument();
  });

  it("groups what came from a pack under the pack's name, after your own", async () => {
    withFlags({ automations: true });
    const fromPack: DeploymentView = {
      ...deployment,
      tasks: deployment.tasks.map((t) =>
        t.taskId === "task-b" ? { ...t, tags: ["gents:pack:demo/research"] } : t,
      ),
    };
    expect(
      byPack(automationsOf(fromPack)).map((g) => [g.pack, g.items.length]),
    ).toEqual([
      [null, 1],
      ["demo/research", 1],
    ]);
    const app = testApp({
      snapshot: { bootstrap, client: { deployments: [fromPack] } },
    });
    renderIn(app, <AutomationsScreen />);
    const group = screen.getByRole("group", { name: "From demo/research" });
    expect(within(group).getByRole("link", { name: /Task B/ })).toBeInTheDocument();
    await act(async () => {
      fireEvent.click(within(group).getByRole("button", { expanded: true }));
    });
    expect(
      within(group).queryByRole("link", { name: /Task B/ }),
    ).not.toBeInTheDocument();
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
    expect(screen.getByRole("radio", { name: /On a schedule/ })).toBeChecked();
    expect(screen.getByRole("radio", { name: "Weekdays · 9:00" })).toBeChecked();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    await act(async () => {
      fireEvent.click(screen.getByRole("radio", { name: "Hourly" }));
    });
    expect(screen.getByRole("radio", { name: "Hourly" })).toBeChecked();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Create" }));
    });
    expect(changeConfig).toHaveBeenCalled();
    expect(screen.queryByLabelText("What the agent should do")).not.toBeInTheDocument();
  });

  it("asks when a blank automation runs instead of assuming, and drops the ask once answered", async () => {
    withFlags({ automations: true });
    const changeConfig = vi.fn().mockResolvedValue({});
    const app = testApp({
      api: { applyConfigComponents: changeConfig },
      snapshot: { bootstrap, client: { deployments: [deployment] } },
    });
    renderRouted(app, { name: "automations", item: "new" }, (route) =>
      route.name === "automations" ? <AutomationsScreen item={route.item} /> : null,
    );
    for (const radio of screen.getAllByRole("radio")) expect(radio).not.toBeChecked();
    fireEvent.change(screen.getByLabelText("What the agent should do"), {
      target: { value: "Tidy the inbox" },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Create" }));
    });
    expect(screen.getByText("Choose when it runs.")).toBeInTheDocument();
    expect(changeConfig).not.toHaveBeenCalled();
    await act(async () => {
      fireEvent.click(screen.getByRole("radio", { name: /Only when I start it/ }));
    });
    expect(screen.queryByText("Choose when it runs.")).not.toBeInTheDocument();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Create" }));
    });
    expect(changeConfig).toHaveBeenCalledWith(
      expect.objectContaining({
        document: expect.not.objectContaining({ triggers: expect.anything() }),
      }),
    );
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
