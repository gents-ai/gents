import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { NodeAgentStack } from "../src/ui/screens/NodeBehaviorStack";
import { node, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const agent = (agentId: string, displayName: string) => ({
  ...deployment.agents[0],
  agentId,
  displayName,
});

const worker = (sessionId: string, agentId: string) =>
  ({
    sessionId,
    nodeDid: "did:key:here",
    agentId,
    title: null,
    turnState: null,
    updatedAt: null,
  }) as SessionSummary;

const names = ["Default", "Code Reviewer", "Planner", "Tester", "Writer", "Mapper"];
const ids = names.map((name) => name.toLowerCase().replace(" ", "-"));

const app = () =>
  testApp({
    deployments: [
      node({
        nodeDid: "did:key:here",
        agents: names.map((name, i) => agent(ids[i]!, name)),
      }),
    ],
    selection: { nodeDid: "did:key:here" },
  });

describe("an agent stack", () => {
  it("shows the session's agent alone, with no count, when every worker runs it", () => {
    const { container } = renderIn(
      app(),
      <NodeAgentStack
        nodeDid="did:key:here"
        agentId="default"
        workers={[1, 2, 3, 4, 5].map((n) => worker(`w${n}`, "default"))}
      />,
    );
    expect(screen.getByText("De")).toBeInTheDocument();
    expect(container).not.toHaveTextContent(/\+\d/);
  });

  it("shows each distinct agent its workers use, once", () => {
    const { container } = renderIn(
      app(),
      <NodeAgentStack
        nodeDid="did:key:here"
        agentId="default"
        workers={[
          worker("w1", "code-reviewer"),
          worker("w2", "code-reviewer"),
          worker("w3", "default"),
        ]}
      />,
    );
    expect(screen.getByText("De")).toBeInTheDocument();
    expect(screen.getAllByText("Cr")).toHaveLength(1);
    expect(container).not.toHaveTextContent(/\+\d/);
  });

  it("counts only the agents left out past three", () => {
    renderIn(
      app(),
      <NodeAgentStack
        nodeDid="did:key:here"
        agentId="default"
        workers={ids.slice(1).map((id, i) => worker(`w${i}`, id))}
      />,
    );
    expect(screen.getByTitle("2 more agents")).toHaveTextContent("+2");
  });
});
