import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { describe, expect, it } from "vitest";

import { NavPanel } from "@/app/RailFlyout";
import { switchedRoute } from "@/app/NodeSwitcher";
import { deployment } from "./config-panel-wiring/fixtures";
import { node, renderIn, testApp } from "./app-fixture";

describe("switching node", () => {
  it("keeps to the same page on the new node, or starts a new session", () => {
    expect(
      switchedRoute({ name: "agent", nodeDid: "a", section: "skills", item: "s" }, "b"),
    ).toEqual({ name: "agent", nodeDid: "b", section: "skills" });
    expect(switchedRoute({ name: "sessions", nodeDid: "a" }, "b")).toEqual({
      name: "sessions",
      nodeDid: "b",
    });
    expect(switchedRoute({ name: "session", sessionId: "s1" }, "b")).toEqual({
      name: "session",
      sessionId: null,
    });
    expect(switchedRoute({ name: "mailbox" }, "b")).toBeNull();
  });

  it("lists the nodes from the nav's node row and makes the chosen one active", async () => {
    const app = testApp({
      deployments: [
        node(),
        node({
          nodeDid: "did:key:z6MkJuno",
          label: "Juno",
          source: "enrollment",
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
    const juno = await screen.findByRole("menuitemradio", { name: /Juno/ });
    /* this machine's node first, then the remote ones, each under a heading */
    expect(screen.getByText("Local")).toBeVisible();
    expect(screen.getByText("Remote")).toBeVisible();
    expect(juno).toHaveAttribute("aria-checked", "false");
    await user.click(juno);
    await waitFor(() =>
      expect(app.stores.selection.getState().nodeDid).toBe("did:key:z6MkJuno"),
    );
    /* choosing closes the menu */
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  });
});
