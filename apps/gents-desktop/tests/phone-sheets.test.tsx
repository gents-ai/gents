import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { NavPanel } from "@/app/RailFlyout";
import { emptyFilter } from "../src/ui/lib/session-filter";
import { SessionFilters } from "../src/ui/screens/SessionFilters";
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

  it("filters sessions from one button and a sheet of every axis", async () => {
    const onChange = vi.fn();
    renderIn(
      testApp({ deployments: [node()], selection: { nodeDid: deployment.nodeDid } }),
      <SessionFilters
        sessions={[{ sessionId: "s", nodeDid: deployment.nodeDid } as SessionSummary]}
        nodeDids={[deployment.nodeDid]}
        value={emptyFilter}
        onChange={onChange}
      />,
    );
    expect(screen.queryByLabelText("Session filters")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Filter sessions" }));
    const sheet = await screen.findByRole("dialog", { name: "Filter sessions" });
    expect(within(sheet).getByText("State")).toBeInTheDocument();
    expect(within(sheet).getByText("Started by")).toBeInTheDocument();
    const [first] = within(sheet).getAllByRole("button", { pressed: false });
    await userEvent.click(first!);
    expect(onChange).toHaveBeenCalledOnce();
  });
});
