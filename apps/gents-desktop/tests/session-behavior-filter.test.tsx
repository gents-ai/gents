import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import {
  emptyFilter,
  hasFilter,
  passes,
  restoreSessionFilter,
} from "../src/ui/lib/session-filter";
import { resetSessionViews, seenMarks } from "../src/ui/app/sessionViews";
import { sessionKeyOf } from "../src/hooks/fleetStore";
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

describe("the new since you looked filter", () => {
  beforeEach(resetSessionViews);

  it("counts the sessions with new work and narrows to them when turned on", async () => {
    const fresh = session({
      sessionId: "fresh",
      latestRequestId: "r2",
      requesterDid: null,
      startedBy: null,
    });
    const known = session({
      sessionId: "known",
      latestRequestId: "r1",
      requesterDid: null,
      startedBy: null,
    });
    seenMarks.set(sessionKeyOf(fresh), { request: "r1", done: true });
    seenMarks.set(sessionKeyOf(known), { request: "r1", done: true });
    const onChange = vi.fn();
    renderIn(
      app(),
      <SessionFilters
        sessions={[fresh, known]}
        nodeDids={["did:key:there"]}
        value={emptyFilter}
        onChange={onChange}
      />,
    );
    const toggle = screen.getByTestId("fresh-filter");
    expect(toggle).toHaveAttribute("aria-pressed", "false");
    await userEvent.click(toggle);
    expect(onChange).toHaveBeenCalledWith({ ...emptyFilter, fresh: true });

    const isFresh = (c: SessionSummary) => c.sessionId === "fresh";
    const on = { ...emptyFilter, fresh: true };
    expect([fresh, known].filter((c) => passes(c, on, isFresh))).toEqual([fresh]);
    expect(hasFilter(on)).toBe(true);
    expect(restoreSessionFilter(on)).toEqual(on);
    expect(restoreSessionFilter(emptyFilter)).toEqual(emptyFilter);
  });
});
