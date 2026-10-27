import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";

import { NavPanel } from "@/app/RailFlyout";
import { listViews, restoreListViews } from "@/app/listViews";
import { isShown, toggleShown } from "@/lib/session-filter";
import { preferences } from "@/preferences";
import { sessionKeyOf } from "../src/hooks/fleetStore";
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
  it("keeps the owning node in pinned and recent session links", () => {
    preferences.setPinned(sessionKeyOf(sessions[0]!), true);
    showNav();

    for (const session of sessions) {
      expect(screen.getByText(session.title!).closest("a")).toHaveAttribute(
        "href",
        `/nodes/${encodeURIComponent(session.nodeDid)}/sessions/${session.sessionId}`,
      );
    }
  });

  it("starts with every choice checked, and hides a state when it is unchecked", async () => {
    const { u } = showNav();
    expect(recentTitles()).toEqual(["Busy", "Done"]);

    await u.click(screen.getByRole("button", { name: "Filter recent sessions" }));
    /* a pointer both hovers the submenu open and clicks it, which can close it again */
    (await screen.findByRole("menuitem", { name: /State/ })).focus();
    await u.keyboard("{ArrowRight}");
    await waitFor(() => expect(screen.getAllByRole("menu").length).toBeGreaterThan(1));
    const states = within(screen.getAllByRole("menu").at(-1)!);
    for (const item of states.getAllByRole("menuitemcheckbox")) {
      expect(item).toHaveAttribute("aria-checked", "true");
      expect(item).not.toHaveAttribute("aria-disabled");
    }
    await u.click(states.getByRole("menuitemcheckbox", { name: /Finished/ }));

    await waitFor(() => expect(recentTitles()).toEqual(["Busy"]));
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

  it("keeps the rest when one is unchecked, and clears when all are checked again", () => {
    const all = ["a", "b", "c"] as const;
    expect(toggleShown(all, [], "b")).toEqual(["a", "c"]);
    expect(toggleShown(all, ["a", "c"], "b")).toEqual([]);
    expect(toggleShown(all, ["a"], "a")).toEqual(["a"]);
    expect(isShown([], "a")).toBe(true);
    expect(isShown(["b"], "a")).toBe(false);
  });
});
