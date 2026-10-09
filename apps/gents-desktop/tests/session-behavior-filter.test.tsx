import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { emptyFilter } from "../src/ui/lib/session-filter";
import { SessionFilters } from "../src/ui/screens/SessionFilters";
import { node, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const session = (over: Partial<SessionSummary>) =>
  ({
    sessionId: "s",
    nodeDid: "did:key:there",
    agentId: null,
    title: null,
    turnState: "completed",
    updatedAt: null,
    ...over,
  }) as SessionSummary;

/* two nodes; the list shows a session on the second, which runs the reviewer */
const app = () =>
  testApp({
    deployments: [
      node({ nodeDid: "did:key:here" }),
      node({
        nodeDid: "did:key:there",
        agents: [
          {
            ...deployment.agents[0],
            agentId: "reviewer",
            displayName: "Code Reviewer",
          },
        ],
      }),
    ],
    selection: { nodeDid: "did:key:here" },
  });

describe("the sessions agent filter", () => {
  it("offers every agent of the nodes the list spans, named by their own node", async () => {
    renderIn(
      app(),
      <SessionFilters
        sessions={[session({ agentId: "reviewer" })]}
        nodeDids={["did:key:here", "did:key:there"]}
        value={emptyFilter}
        onChange={vi.fn()}
      />,
    );
    await userEvent.click(screen.getByLabelText("Agent"));
    expect(await screen.findByText("Code Reviewer")).toBeInTheDocument();
    /* the selected node's agent runs no listed session; it is offered at zero */
    expect(screen.getByRole("option", { name: /Default\s*0$/ })).toBeInTheDocument();
  });

  it("keeps a stored pick no listed session runs in view, so it can be cleared", () => {
    renderIn(
      app(),
      <SessionFilters
        sessions={[session({ agentId: "reviewer" })]}
        nodeDids={["did:key:here", "did:key:there"]}
        value={{ ...emptyFilter, agents: ["gone"] }}
        onChange={vi.fn()}
      />,
    );
    expect(screen.getByLabelText("Agent")).toHaveTextContent("gone");
  });
});
