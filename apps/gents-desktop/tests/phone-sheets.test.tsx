import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { AppShell } from "@/app/AppShell";
import { NavPanel } from "@/app/RailFlyout";
import { emptyFilter, type SessionFilter } from "@/lib/session-filter";
import { SessionFilters } from "@/screens/SessionFilters";
import { deployment } from "./config-panel-wiring/fixtures";
import { node, renderIn, testApp } from "./app-fixture";

/* a window narrower than 768 px: only the narrow query matches */
const realMatchMedia = window.matchMedia;
beforeEach(() => {
  window.matchMedia = ((query: string) =>
    ({
      matches: query === "(max-width: 767px)",
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as MediaQueryList) as typeof window.matchMedia;
});
afterEach(() => {
  window.matchMedia = realMatchMedia;
});

describe("on a narrow window", () => {
  it("switches node from a sheet of full-width rows", async () => {
    const app = testApp({
      deployments: [
        node(),
        node({
          nodeDid: "did:key:z6MkJuno",
          label: "Juno",
          local: false,
          node: { ...deployment.node, nodeDid: "did:key:z6MkJuno", displayName: null },
        }),
      ],
      selection: { nodeDid: deployment.nodeDid },
    });
    renderIn(
      app,
      <TooltipProvider>
        <NavPanel route={{ name: "mailbox" }} settings={null} syncRow={false} />
      </TooltipProvider>,
    );
    const user = userEvent.setup({ pointerEventsCheck: 0 });
    await user.click(screen.getByRole("button", { name: "Switch node" }));
    const sheet = await screen.findByRole("dialog", { name: "Switch node" });
    expect(within(sheet).queryAllByRole("menuitemradio")).toHaveLength(0);
    const juno = within(sheet).getByRole("button", { name: /Juno/ });
    expect(juno).toHaveAttribute("aria-pressed", "false");
    await user.click(juno);
    expect(app.stores.selection.getState().nodeDid).toBe("did:key:z6MkJuno");
  });

  it("filters sessions from one button's sheet, and resets them there", async () => {
    const sessions = [
      { sessionId: "a", nodeDid: deployment.nodeDid, turnState: "completed" },
      { sessionId: "b", nodeDid: deployment.nodeDid, turnState: "failed" },
    ] as SessionSummary[];
    let filter: SessionFilter = emptyFilter;
    function Filters() {
      const [value, setValue] = useState(emptyFilter);
      filter = value;
      return (
        <SessionFilters
          sessions={sessions}
          nodeDids={[deployment.nodeDid]}
          value={value}
          onChange={setValue}
        />
      );
    }
    renderIn(testApp({ deployments: [node()] }), <Filters />);
    const user = userEvent.setup({ pointerEventsCheck: 0 });

    expect(screen.queryByLabelText("Session filters")).toBeNull();
    expect(screen.queryByTestId("session-filters-active")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Filter sessions" }));
    const sheet = await screen.findByRole("dialog", { name: "Filter sessions" });
    const reset = within(sheet).getByRole("button", { name: "Reset filters" });
    expect(reset).toBeDisabled();

    const state = within(sheet).getByRole("group", { name: "State" });
    const finished = within(state).getByRole("button", { name: /Finished/ });
    expect(finished).toHaveAttribute("aria-pressed", "true");
    await user.click(finished);
    expect(finished).toHaveAttribute("aria-pressed", "false");
    expect(filter.states).not.toContain("finished");
    expect(filter.states).toContain("failed");
    expect(screen.getByTestId("session-filters-active")).toBeInTheDocument();

    await user.click(reset);
    expect(filter).toEqual(emptyFilter);
    expect(finished).toHaveAttribute("aria-pressed", "true");
    expect(screen.queryByTestId("session-filters-active")).toBeNull();
  });

  it("holds the mark, history and sync status in the menu, with a dot on its button", async () => {
    const app = testApp({
      snapshot: {
        bootstrap: {},
        client: {
          deployments: [node()],
          syncHealth: {
            state: "blocked",
            runtimeCount: 1,
            connectedRuntimeCount: 0,
            cause: "runtime_refused",
            nextAction: null,
          },
        },
      },
    });
    const back = vi.fn();
    renderIn(
      app,
      <TooltipProvider>
        <AppShell
          route={{ name: "mailbox" }}
          history={{ canBack: true, canForward: false, back, forward: () => {} }}
        >
          <p>Content</p>
        </AppShell>
      </TooltipProvider>,
    );
    const user = userEvent.setup({ pointerEventsCheck: 0 });
    const bar = screen.getByTestId("window-bar");
    expect(within(bar).queryByRole("link", { name: "Nodes" })).toBeNull();
    expect(within(bar).queryByRole("button", { name: "Back" })).toBeNull();
    expect(
      within(bar).queryByRole("button", { name: /connection details/ }),
    ).toBeNull();
    expect(within(bar).getByTestId("menu-sync-dot")).toHaveAttribute(
      "data-state",
      "blocked",
    );

    await user.click(within(bar).getByRole("button", { name: "Menu" }));
    const menu = await screen.findByRole("dialog", { name: "Sidebar" });
    expect(within(menu).getAllByRole("link", { name: "Nodes" })[0]).toHaveClass(
      "bg-ink",
    );
    await user.click(
      within(menu).getByRole("button", { name: "Refused. Show connection details." }),
    );
    expect(
      within(menu).getByRole("region", { name: "Connection details" }),
    ).toHaveTextContent(/does not accept this app's credentials/);
    expect(screen.getAllByRole("dialog")).toHaveLength(1);

    await user.click(within(menu).getByRole("button", { name: "Back" }));
    expect(back).toHaveBeenCalledOnce();
  });

  it("gathers search with the mark and history in the menu's top row, and has no close button", async () => {
    const app = testApp({
      snapshot: { bootstrap: {}, client: { deployments: [node()] } },
    });
    renderIn(
      app,
      <TooltipProvider>
        <AppShell
          route={{ name: "mailbox" }}
          history={{
            canBack: true,
            canForward: true,
            back: () => {},
            forward: () => {},
          }}
        >
          <p>Content</p>
        </AppShell>
      </TooltipProvider>,
    );
    const user = userEvent.setup({ pointerEventsCheck: 0 });
    const bar = screen.getByTestId("window-bar");
    await user.click(within(bar).getByRole("button", { name: "Menu" }));
    const menu = await screen.findByRole("dialog", { name: "Sidebar" });

    const top = within(menu).getByTestId("history-nav");
    expect(within(top).getByRole("button", { name: "Search" })).toBeVisible();
    expect(within(top).getByRole("button", { name: "Back" })).toBeVisible();
    expect(within(top).getByRole("button", { name: "Forward" })).not.toHaveClass(
      "max-md:hidden",
    );
    /* one search control, in that row, and no row of its own */
    expect(within(menu).getAllByRole("button", { name: "Search" })).toHaveLength(1);
    expect(within(menu).queryByRole("button", { name: "Close" })).toBeNull();

    /* search opens over the page, so the menu gets out of its way */
    await user.click(within(top).getByRole("button", { name: "Search" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog", { name: "Sidebar" })).toBeNull(),
    );
  });
});
