import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { afterEach, describe, expect, it } from "vitest";

import { NavNodes } from "@/app/NavNodes";
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
          mailboxItems: ["a", "b"].map((itemId) => ({
            itemId,
            itemKey: itemId,
            requesterDid: "did:key:person",
            nodeDid: "did:key:z6MkJuno",
            status: "open",
            kind: "finished",
            action: "ack",
            title: "Done",
            summary: null,
            payload: null,
            sourceKind: "session",
            sourceId: "juno-1",
            sessionId: "juno-1",
            requestId: null,
            causeDocId: null,
            targetNodeDid: "did:key:z6MkJuno",
            targetAgentDid: null,
            expectedCollection: null,
            parentItemId: null,
            deadlineAt: null,
            createdAt: "2026-11-01T10:00:00Z",
          })),
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
    /* what it has going on, before switching to it, as marks named in words */
    expect(
      within(juno).getByRole("img", { name: "1 running session" }),
    ).toHaveTextContent("1");
    expect(
      within(juno).getByRole("img", { name: "2 mailbox items" }),
    ).toHaveTextContent("2");
    /* an idle node shows no marks */
    const idle = screen.getAllByRole("menuitemradio").filter((item) => item !== juno);
    expect(idle).not.toHaveLength(0);
    for (const item of idle)
      expect(
        within(item).queryAllByRole("img", { name: /running|mailbox/ }),
      ).toHaveLength(0);
    await user.click(juno);
    await waitFor(() =>
      expect(app.stores.selection.getState().nodeDid).toBe("did:key:z6MkJuno"),
    );
    /* choosing closes the menu */
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  });

  it("shows the same marks on the sidebar's node rows", () => {
    renderIn(
      testApp({
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
            node: {
              ...deployment.node,
              nodeDid: "did:key:z6MkJuno",
              displayName: null,
            },
          }),
        ],
        selection: { nodeDid: deployment.nodeDid },
      }),
      <TooltipProvider>
        <NavNodes route={{ name: "mailbox" }} current={deployment.nodeDid} />
      </TooltipProvider>,
    );
    const juno = screen.getByRole("button", { name: /Juno/ });
    expect(
      within(juno).getByRole("img", { name: "1 running session" }),
    ).toHaveTextContent("1");
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

  it("shows the node's card from New session's avatar", async () => {
    renderIn(
      twoNodes(),
      <TooltipProvider>
        <NewSession />
      </TooltipProvider>,
    );
    const avatar = screen
      .getByTestId("session-screen")
      .querySelector(":scope > div > span");
    await userEvent.hover(avatar!);
    expect(await screen.findByText("Inference", {}, { timeout: 2000 })).toBeVisible();
  });
});
