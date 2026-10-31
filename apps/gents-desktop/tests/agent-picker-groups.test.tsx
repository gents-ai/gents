import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AgentPicker } from "../src/ui/screens/BehaviorPicker";
import { renderIn, testApp } from "./app-fixture";
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

const open = async (agentId = deployment.agents[0]!.agentId) => {
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
      agent(deployment.agents[0]!.agentId, "The Engineer", { isDefault: true }),
    ].map((a) => ({ ...deployment.agents[0]!, ...a })),
  };
  renderIn(
    testApp(),
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
    ]);
    const header = within(list).getByRole("button", { name: /demo · 1/ });
    expect(header).toHaveAttribute("aria-expanded", "false");
    await userEvent.setup().click(header);
    expect(within(list).getByRole("option", { name: /Pack worker/ })).toBeTruthy();
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
