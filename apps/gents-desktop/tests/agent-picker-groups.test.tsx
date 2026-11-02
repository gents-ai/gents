import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AgentPicker } from "../src/ui/screens/BehaviorPicker";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { node, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

afterEach(() => vi.unstubAllGlobals());

const agent = (agentId: string, displayName: string, extra = {}) => ({
  agentId,
  displayName,
  description: `${displayName} does things.`,
  enabled: true,
  isDefault: false,
  tags: [],
  ...extra,
});

const session = (sessionId: string, agentId: string, createdAt: string, over = {}) =>
  ({
    sessionId,
    nodeDid: deployment.nodeDid,
    agentId,
    createdAt,
    startedBy: null,
    taskId: null,
    triggerId: null,
    ...over,
  }) as SessionSummary;

const open = async (
  agentId = deployment.agents[0]!.agentId,
  sessions: SessionSummary[] = [],
) => {
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => store.set(key, value),
  });
  const view = {
    ...deployment,
    agents: [
      agent("worker", "Pack worker", { tags: ["gents:pack:demo"] }),
      agent("lister", "Lister"),
      agent("reviewer", "Reviewer"),
      agent(deployment.agents[0]!.agentId, "The Engineer", { isDefault: true }),
    ].map((a) => ({ ...deployment.agents[0]!, ...a })),
  };
  renderIn(
    testApp({ deployments: [node({ sessions })] }),
    <AgentPicker deployment={view} agentId={agentId} onChange={vi.fn()} />,
  );
  await userEvent.setup().click(screen.getByRole("button", { name: "Agent" }));
  return screen.getByRole("listbox", { name: "Agent" });
};

describe("the agent picker", () => {
  it("lists the person's own agents first, the default leading, and folds a pack's", async () => {
    const list = await open();
    const options = within(list).getAllByRole("option");
    expect(options.map((o) => o.textContent)).toEqual([
      expect.stringContaining("The Engineer"),
      expect.stringContaining("Lister"),
      expect.stringContaining("Reviewer"),
    ]);
    expect(within(list).queryByRole("group", { name: "Recent" })).toBeNull();
    const header = within(list).getByRole("button", { name: /demo · 1/ });
    expect(header).toHaveAttribute("aria-expanded", "false");
    await userEvent.setup().click(header);
    expect(within(list).getByRole("option", { name: /Pack worker/ })).toBeTruthy();
  });

  it("lifts the agents the person used lately under the default, apart from the rest", async () => {
    const list = await open(undefined, [
      session("s1", "lister", "2026-11-01T09:00:00Z"),
      session("s2", "reviewer", "2026-11-01T10:00:00Z"),
      session("s3", deployment.agents[0]!.agentId, "2026-11-01T11:00:00Z"),
      /* started by another session or a task: not the person's pick */
      session("s4", "worker", "2026-11-01T12:00:00Z", {
        startedBy: { sessionId: "s2" },
      }),
      session("s5", "worker", "2026-11-01T12:30:00Z", { taskId: "task-a" }),
    ]);
    const recent = within(list).getByRole("group", { name: "Recent" });
    expect(
      within(recent)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual([expect.stringContaining("Reviewer"), expect.stringContaining("Lister")]);
    const options = within(list).getAllByRole("option");
    expect(options.map((o) => o.textContent)).toEqual([
      expect.stringContaining("The Engineer"),
      expect.stringContaining("Reviewer"),
      expect.stringContaining("Lister"),
    ]);
  });

  it("keeps a recent pack agent in its pack too, and drops Recent while searching", async () => {
    const user = userEvent.setup();
    const list = await open(undefined, [
      session("s1", "worker", "2026-11-01T09:00:00Z"),
    ]);
    const recent = within(list).getByRole("group", { name: "Recent" });
    expect(within(recent).getByRole("option", { name: /Pack worker/ })).toBeTruthy();
    expect(within(list).getByRole("button", { name: /demo · 1/ })).toBeTruthy();

    await user.type(screen.getByRole("textbox", { name: "Search agents" }), "er");
    expect(within(list).queryByRole("group", { name: "Recent" })).toBeNull();
    expect(within(list).getAllByRole("option", { name: /Pack worker/ })).toHaveLength(
      1,
    );
  });

  it("keeps each agent to one line, its model and access in its tooltip", async () => {
    const list = await open();
    const engineer = within(list).getByRole("option", { name: /The Engineer/ });
    expect(engineer).toHaveTextContent("The Engineerdefault");
    expect(engineer.title).toMatch(/files .* · bash .* · net /);
  });

  it("opens the group that holds the chosen agent", async () => {
    const list = await open("worker");
    expect(within(list).getByRole("option", { name: /Pack worker/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });
});
