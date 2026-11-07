import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { emptyFilter, matchesSource } from "../src/ui/lib/session-filter";
import { SessionFilters } from "../src/ui/screens/SessionFilters";
import { StartedByAutomation } from "../src/ui/screens/SessionHeader";
import { href } from "../src/ui/lib/router";
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

describe("the sessions source filter", () => {
  it("recognizes a task inherited from a different node", () => {
    const worker = session({
      taskId: null,
      taskOrigin: { nodeDid: "did:key:origin", taskId: "review", taskName: "Review" },
    });
    expect(matchesSource(worker, "task")).toBe(true);
    expect(matchesSource(worker, "person")).toBe(false);
  });

  it("opens the task on its originating node", () => {
    const worker = session({
      taskId: null,
      taskOrigin: { nodeDid: "did:key:origin", taskId: "review", taskName: "Review" },
    });
    render(<StartedByAutomation summary={worker} nodeDid="did:key:worker" inherited />);
    expect(screen.getByText("From task")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Review" })).toHaveAttribute(
      "href",
      href({
        name: "agent",
        nodeDid: "did:key:origin",
        section: "tasks",
        item: "review",
      }),
    );
  });
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
    expect(screen.getByRole("option", { name: /Default$/ })).toBeInTheDocument();
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
