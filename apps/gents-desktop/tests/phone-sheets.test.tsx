import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { NavPanel } from "@/app/RailFlyout";
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
});
