import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { afterEach, describe, expect, it } from "vitest";

import { NavPanel } from "@/app/RailFlyout";
import { switchedRoute } from "@/app/NodeSwitcher";
import { bindNavigationGuard } from "@/lib/router";
import { NewSession } from "@/screens/session/NewSession";
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
          sessions: [
            {
              sessionId: "juno-1",
              nodeDid: "did:key:z6MkJuno",
              title: "Busy",
              turnState: "running",
              updatedAt: "2026-11-01T10:00:00Z",
            },
          ],
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
    /* what it has going on, before switching to it */
    expect(juno).toHaveTextContent("1 running");
    await user.click(juno);
    await waitFor(() =>
      expect(app.stores.selection.getState().nodeDid).toBe("did:key:z6MkJuno"),
    );
    /* choosing closes the menu */
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  });

  const twoNodes = () =>
    testApp({
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

  let unbind: (() => void) | null = null;
  afterEach(() => {
    unbind?.();
    unbind = null;
  });

  it("asks first, as a link to another node's page would", async () => {
    const app = twoNodes();
    const asked: unknown[] = [];
    unbind = bindNavigationGuard((route) => {
      asked.push(route);
      return false;
    });
    renderIn(
      app,
      <TooltipProvider>
        <NavPanel route={{ name: "mailbox" }} settings={null} syncRow={false} />
      </TooltipProvider>,
    );
    const user = userEvent.setup({ pointerEventsCheck: 0 });
    await user.click(screen.getByRole("button", { name: "Switch node" }));
    await user.click(await screen.findByRole("menuitemradio", { name: /Juno/ }));
    expect(asked).toEqual([{ name: "sessions", nodeDid: "did:key:z6MkJuno" }]);
    /* refused, so the node stays */
    expect(app.stores.selection.getState().nodeDid).toBe(deployment.nodeDid);
  });

  it("switches from the node's name in New session's heading", async () => {
    const app = twoNodes();
    renderIn(
      app,
      <TooltipProvider>
        <NewSession />
      </TooltipProvider>,
    );
    const user = userEvent.setup({ pointerEventsCheck: 0 });
    await user.click(screen.getByTitle("Choose a node"));
    await user.click(await screen.findByRole("menuitemradio", { name: /Juno/ }));
    await waitFor(() =>
      expect(app.stores.selection.getState().nodeDid).toBe("did:key:z6MkJuno"),
    );
  });
});
