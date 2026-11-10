import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { fleetNodes } from "../src/ui/lib/scope";
import { AgentPicker } from "../src/ui/screens/BehaviorPicker";
import { AgentHoverCard } from "../src/ui/screens/HoverCards";
import { NodeAgentStack } from "../src/ui/screens/NodeBehaviorStack";
import { WorkerAgents } from "../src/ui/screens/WorkerAgents";
import { agentHue, agentName, didTail } from "../src/ui/screens/behavior";
import { AgentInitials } from "../src/ui/screens/parts";
import { node, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

afterEach(() => vi.unstubAllGlobals());

const FIRST = "did:key:z6MkReviewerFirst111aaa";
const SECOND = "did:key:z6MkReviewerSecond222bbb";

const agent = (agentDid: string, displayName: string, extra = {}) => ({
  ...deployment.agents[0]!,
  agentDid,
  displayName,
  description: "",
  enabled: true,
  isDefault: false,
  tags: [],
  ...extra,
});

const agents = [
  agent("did:key:z6MkLead", "Lead", { isDefault: true }),
  agent(FIRST, "Reviewer"),
  agent(SECOND, "reviewer"),
  agent("did:key:z6MkWriter", "Writer"),
];

const hueOf = (el: Element) =>
  (el as HTMLElement).style.getPropertyValue("--agent-hue");

describe("an agent's name", () => {
  const view = { ...deployment, agents } as never;

  it("is the name its node lists, the default agent's own included", () => {
    expect(agentName("did:key:z6MkLead", view)).toBe("Lead");
  });

  it("is unknown, with its DID's tail, when its node does not list it", () => {
    expect(agentName("did:key:z6MkGone123456", view)).toBe("Unknown agent …123456");
    expect(agentName("did:key:z6MkGone123456", null)).toBe("Unknown agent …123456");
    expect(agentName(null, view)).toBe("Unknown agent");
  });

  it("draws an unknown agent as a question tinted by its DID, not as initials", () => {
    const did = "did:key:z6MkGone123456";
    render(<AgentInitials name={agentName(did, null)} agentDid={did} />);
    expect(hueOf(screen.getByText("?"))).toBe(String(agentHue(did)));
  });
});

describe("agents that share a name", () => {
  it("are tinted by their DIDs, each keeping its own", () => {
    expect(agentHue(FIRST)).toBe(agentHue(FIRST));
    expect(agentHue(FIRST)).not.toBe(agentHue(SECOND));
    render(
      <>
        <AgentInitials name="Reviewer" agentDid={FIRST} shared data-testid="a" />
        <AgentInitials name="Reviewer" agentDid={SECOND} shared data-testid="b" />
        <AgentInitials name="Writer" agentDid="did:key:z6MkWriter" data-testid="c" />
      </>,
    );
    expect(hueOf(screen.getByTestId("a"))).toBe(String(agentHue(FIRST)));
    expect(hueOf(screen.getByTestId("b"))).toBe(String(agentHue(SECOND)));
    expect(hueOf(screen.getByTestId("c"))).toBe("");
  });

  it("show their DIDs' tails in the agent picker; a unique name shows none", async () => {
    vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {} });
    renderIn(
      testApp({ deployments: [node()] }),
      <AgentPicker
        deployment={{ ...deployment, agents } as never}
        agentDid={FIRST}
        onChange={vi.fn()}
      />,
    );
    const trigger = screen.getByRole("button", { name: "Agent" });
    expect(trigger).toHaveTextContent(`Reviewer${didTail(FIRST)}`);
    await userEvent.setup().click(trigger);
    const list = screen.getByRole("listbox", { name: "Agent" });
    const option = (name: RegExp) => within(list).getByRole("option", { name });
    expect(option(/^Reviewer/)).toHaveTextContent(didTail(FIRST));
    expect(option(/^reviewer/)).toHaveTextContent(didTail(SECOND));
    expect(option(/^Writer/)).not.toHaveTextContent("…");
    expect(option(/^Lead/)).not.toHaveTextContent("…");
  });

  it("differ in a worker stack", () => {
    render(
      <WorkerAgents
        agents={[
          { agentDid: FIRST, displayName: "Reviewer" },
          { agentDid: SECOND, displayName: "Reviewer" },
          { agentDid: "did:key:z6MkWriter", displayName: "Writer" },
        ]}
      />,
    );
    const [first, second] = screen.getAllByText("Re");
    expect(hueOf(first!)).toBe(String(agentHue(FIRST)));
    expect(hueOf(second!)).toBe(String(agentHue(SECOND)));
    expect(hueOf(screen.getByText("Wr"))).toBe("");
  });

  it("differ beside the session's agent in its stack", () => {
    const worker = {
      sessionId: "w1",
      nodeDid: "did:key:here",
      agentDid: SECOND,
      title: null,
      turnState: null,
      updatedAt: null,
    } as SessionSummary;
    renderIn(
      testApp({
        deployments: [
          node({
            nodeDid: "did:key:here",
            agents: [agent(FIRST, "Reviewer"), agent(SECOND, "Reviewer")],
          }),
        ],
      }),
      <NodeAgentStack nodeDid="did:key:here" agentDid={FIRST} workers={[worker]} />,
    );
    const hues = screen.getAllByText("Re").map(hueOf);
    expect(hues).toEqual([String(agentHue(FIRST)), String(agentHue(SECOND))]);
  });

  it("show their DIDs' tails on the agent's card", async () => {
    const app = testApp({
      deployments: [
        node({ agents: [agent(FIRST, "Reviewer"), agent(SECOND, "Reviewer")] }),
      ],
    });
    const [shown] = fleetNodes(app.stores.fleet.getState());
    renderIn(
      app,
      <AgentHoverCard deployment={shown!} agentDid={SECOND}>
        <button type="button">agent</button>
      </AgentHoverCard>,
    );
    await userEvent.hover(screen.getByRole("button", { name: "agent" }));
    const card = await screen.findByTestId("agent-hover-card", {}, { timeout: 2000 });
    expect(card).toHaveTextContent(didTail(SECOND));
    expect(card).not.toHaveTextContent(didTail(FIRST));
  });
});
