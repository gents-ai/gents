import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";

import { NavPanel } from "@/app/RailFlyout";
import {
  listViews,
  migrateListViews,
  recentDefault,
  restoreListViews,
} from "@/app/listViews";
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
  summary({
    sessionId: "worker",
    title: "Worker",
    startedBy: {
      nodeDid: deployment.nodeDid,
      sessionId: "busy",
      requesterDid: null,
      causeRequestDocId: "request-busy",
    },
    updatedAt: "2026-10-26T12:00:00Z",
  }),
];

function showNav(list: SessionSummary[] = sessions) {
  renderIn(
    testApp({ deployments: [node({ sessions: list })] }),
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
    .filter((t) => t === "Done" || t === "Busy" || t === "Worker");

describe("the side nav's recent sessions filter", () => {
  it("keeps the owning node in pinned and recent session links", () => {
    preferences.setPinned(sessionKeyOf(sessions[0]!), true);
    showNav();

    /* the worker is left out of the recent list */
    for (const session of sessions.filter((s) => !s.startedBy)) {
      expect(screen.getByText(session.title!).closest("a")).toHaveAttribute(
        "href",
        `/nodes/${encodeURIComponent(session.nodeDid)}/sessions/${session.sessionId}`,
      );
    }
  });

  it("leaves out sessions another session started until the menu shows them", async () => {
    const { u } = showNav();
    expect(recentTitles()).toEqual(["Busy", "Done"]);
    const menu = screen.getByRole("button", { name: "Recent sessions options" });
    expect(menu).not.toHaveClass("text-foreground");

    await u.click(menu);
    const show = await screen.findByRole("menuitemcheckbox", {
      name: "Show sub-sessions",
    });
    expect(show).toHaveAttribute("aria-checked", "false");
    await u.click(show);

    await waitFor(() => expect(recentTitles()).toEqual(["Worker", "Busy", "Done"]));
    expect(menu).toHaveClass("text-foreground");
    /* marked by its starter, not nested; the others unmarked */
    expect(screen.getByRole("img", { name: "Started by Busy" })).toBeInTheDocument();
    expect(screen.getAllByRole("img", { name: /^Started by/ })).toHaveLength(1);
  });

  it("says another session started a sub-session whose starter isn't listed", async () => {
    const { u } = showNav([
      ...sessions,
      summary({
        sessionId: "orphan",
        title: "Orphan",
        startedBy: {
          nodeDid: deployment.nodeDid,
          sessionId: "gone",
          requesterDid: null,
          causeRequestDocId: "request-gone",
        },
      }),
    ]);
    await u.click(screen.getByRole("button", { name: "Recent sessions options" }));
    await u.click(
      await screen.findByRole("menuitemcheckbox", { name: "Show sub-sessions" }),
    );
    expect(
      await screen.findByRole("img", { name: "Started by another session" }),
    ).toBeInTheDocument();
  });

  it("offers every agent on the node, those without recent sessions too", async () => {
    renderIn(
      testApp({
        deployments: [
          node({
            sessions,
            agents: [
              { agentId: "default", displayName: "Default" },
              { agentId: "pack-worker", displayName: "Pack worker" },
            ],
          }),
        ],
        selection: { nodeDid: deployment.nodeDid },
      }),
      <TooltipProvider>
        <NavPanel route={{ name: "sessions" }} settings={null} syncRow={false} />
      </TooltipProvider>,
    );
    const u = userEvent.setup({ pointerEventsCheck: 0 });
    await u.click(screen.getByRole("button", { name: "Recent sessions options" }));
    (await screen.findByRole("menuitem", { name: /Agent/ })).focus();
    await u.keyboard("{ArrowRight}");
    expect(
      await screen.findByRole("menuitemcheckbox", { name: /Pack worker/ }),
    ).toBeInTheDocument();
  });

  it("starts with every state checked, and hides a state when it is unchecked", async () => {
    const { u } = showNav();
    expect(recentTitles()).toEqual(["Busy", "Done"]);

    await u.click(screen.getByRole("button", { name: "Recent sessions options" }));
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

  it("says when nothing matches and resets the filter to leaving out workers", async () => {
    listViews.setRecentFilter({ states: ["failed"], sources: [], agents: [] });
    const { u } = showNav();
    expect(recentTitles()).toEqual([]);
    expect(screen.getByText(/No sessions match/)).toBeInTheDocument();

    await u.click(screen.getByRole("button", { name: "Reset filters" }));
    await waitFor(() => expect(recentTitles()).toEqual(["Busy", "Done"]));
  });

  it("gives an earlier saved filter the new default only where it chose no starter", () => {
    expect(
      migrateListViews(
        { recentFilter: { states: ["working"], sources: [], agents: [] } },
        1,
      ),
    ).toMatchObject({
      recentFilter: { states: ["working"], sources: ["person", "task"], agents: [] },
    });
    const chose = { recentFilter: { states: [], sources: ["session"], agents: [] } };
    expect(migrateListViews(chose, 1)).toBe(chose);
    const current = { recentFilter: { states: [], sources: [], agents: [] } };
    expect(migrateListViews(current, 2)).toBe(current);
  });

  it("restores the nav's filter on its own", () => {
    expect(
      restoreListViews({
        recentFilter: { states: ["working"], sources: [], agents: [] },
      }).recentFilter.states,
    ).toEqual(["working"]);
    expect(restoreListViews({}).recentFilter).toEqual(recentDefault);
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
