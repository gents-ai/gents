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
    agentDid: "did:key:there",
    behaviorId: null,
    title: null,
    turnState: "completed",
    updatedAt: null,
    ...over,
  }) as SessionSummary;

/* two nodes; the list shows a session on the second, which runs the reviewer */
const app = () =>
  testApp({
    deployments: [
      node({ agentDid: "did:key:here" }),
      node({
        agentDid: "did:key:there",
        behaviors: [
          {
            ...deployment.behaviors[0],
            behaviorId: "reviewer",
            displayName: "Code Reviewer",
          },
        ],
      }),
    ],
    selection: { agentDid: "did:key:here" },
  });

describe("the sessions behavior filter", () => {
  it("offers every behavior of the nodes the list spans, named by their own node", async () => {
    renderIn(
      app(),
      <SessionFilters
        sessions={[session({ behaviorId: "reviewer" })]}
        nodeDids={["did:key:here", "did:key:there"]}
        value={emptyFilter}
        onChange={vi.fn()}
      />,
    );
    await userEvent.click(screen.getByLabelText("Behavior"));
    expect(await screen.findByText("Code Reviewer")).toBeInTheDocument();
    /* the selected node's behavior runs no listed session; it is offered at zero */
    expect(screen.getByRole("option", { name: /Default\s*0$/ })).toBeInTheDocument();
  });

  it("keeps a stored pick no listed session runs in view, so it can be cleared", () => {
    renderIn(
      app(),
      <SessionFilters
        sessions={[session({ behaviorId: "reviewer" })]}
        nodeDids={["did:key:here", "did:key:there"]}
        value={{ ...emptyFilter, behaviors: ["gone"] }}
        onChange={vi.fn()}
      />,
    );
    expect(screen.getByLabelText("Behavior")).toHaveTextContent("gone");
  });
});
