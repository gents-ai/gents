import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";

import { NavPanel } from "@/app/RailFlyout";
import { listViews, restoreListViews } from "@/app/listViews";
import { deployment } from "./config-panel-wiring/fixtures";
import { node, renderIn, testApp } from "./app-fixture";

const summary = (overrides: Partial<SessionSummary>): SessionSummary => ({
  sessionId: "one",
  nodeDid: deployment.nodeDid,
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: null,
  latestRequestId: null,
  closedAt: null,
  archivedAt: null,
  archiveRevision: "1",
  tags: [],
  provenance: null,
  title: "One",
  previewText: null,
  status: null,
  agentId: null,
  taskId: null,
  taskName: null,
  triggerId: null,
  triggerKind: null,
  createdAt: null,
  updatedAt: "2026-10-26T10:00:00Z",
  turnState: "completed",
  messageCount: null,
  toolCallCount: null,
  ...overrides,
});

const sessions = [
  summary({ sessionId: "done", title: "Done", turnState: "completed" }),
  summary({
    sessionId: "busy",
    title: "Busy",
    turnState: "running",
    updatedAt: "2026-10-26T11:00:00Z",
  }),
];

function showNav() {
  renderIn(
    testApp({ deployments: [node({ sessions })] }),
    <TooltipProvider>
      <NavPanel route={{ name: "sessions" }} settings={null} syncRow={false} />
    </TooltipProvider>,
  );
  const u = userEvent.setup({ pointerEventsCheck: 0 });
  return { u };
}

const recentTitles = () =>
  screen
    .getAllByRole("link")
    /* a running session's status draws a spinner glyph before its title */
    .map((a) => (a.textContent ?? "").replace(/^[^A-Za-z]+/, ""))
    .filter((t) => t === "Done" || t === "Busy");

describe("the side nav's recent sessions filter", () => {
  it("narrows the recent sessions by state, apart from the sessions list's own", async () => {
    const { u } = showNav();
    expect(recentTitles()).toEqual(["Busy", "Done"]);

    await u.click(screen.getByRole("button", { name: "Filter recent sessions" }));
    await u.click(await screen.findByRole("menuitem", { name: /State/ }));
    await waitFor(() => expect(screen.getAllByRole("menu").length).toBeGreaterThan(1));
    await u.click(
      within(screen.getAllByRole("menu").at(-1)!).getByRole("menuitemcheckbox", {
        name: /Finished/,
      }),
    );

    await waitFor(() => expect(recentTitles()).toEqual(["Done"]));
  });

  it("offers a state no session is in yet", async () => {
    const { u } = showNav();
    await u.click(screen.getByRole("button", { name: "Filter recent sessions" }));
    await u.click(await screen.findByRole("menuitem", { name: /State/ }));
    await waitFor(() => expect(screen.getAllByRole("menu").length).toBeGreaterThan(1));
    const failed = within(screen.getAllByRole("menu").at(-1)!).getByRole(
      "menuitemcheckbox",
      { name: /Failed/ },
    );
    expect(failed).not.toHaveAttribute("aria-disabled");
    await u.click(failed);
    await waitFor(() => expect(recentTitles()).toEqual([]));
  });

  it("says when nothing matches and clears the filter", async () => {
    listViews.setRecentFilter({ states: ["failed"], sources: [], agents: [] });
    const { u } = showNav();
    expect(recentTitles()).toEqual([]);
    expect(screen.getByText(/No sessions match/)).toBeInTheDocument();

    await u.click(screen.getByRole("button", { name: "Clear filters" }));
    await waitFor(() => expect(recentTitles()).toEqual(["Busy", "Done"]));
  });

  it("restores the nav's filter on its own", () => {
    expect(
      restoreListViews({
        recentFilter: { states: ["working"], sources: [], agents: [] },
      }).recentFilter.states,
    ).toEqual(["working"]);
    expect(restoreListViews({}).recentFilter.states).toEqual([]);
  });
});
