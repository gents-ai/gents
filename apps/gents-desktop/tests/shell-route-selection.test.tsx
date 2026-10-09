import { describe, expect, it } from "vitest";

import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { createSelectionActions } from "../src/hooks/selectionActions";
import { selection } from "../src/hooks/selectionStore";
import { shellStores } from "./shell-fixture";

const node = (nodeDid: string, sessionIds: string[]) =>
  ({
    nodeDid,
    node: { defaultAgentId: null },
    agents: [],
    sessions: sessionIds.map((sessionId) => ({ nodeDid, sessionId })),
    mailboxItems: [],
  }) as unknown as DeploymentView;

/* the route's owner over nodes a (session a-1) and b (session b-1) */
function routeOwner(initial = { nodeDid: "a", sessionId: "a-1" as string | null }) {
  const stores = shellStores({
    deployments: [node("a", ["a-1"]), node("b", ["b-1"])],
    selection: initial,
  });
  const route = createSelectionActions({ stores });
  return { store: stores.selection, route };
}

describe("following the route", () => {
  it("selects a session on another node together with its node", () => {
    const { store, route } = routeOwner();
    route.followRoute("b-1");
    expect(store.getState()).toMatchObject({ nodeDid: "b", sessionId: "b-1" });
    /* a snapshot landing afterwards changes nothing */
    const intent = store.getState().intent;
    route.followRoute("b-1", true);
    expect(store.getState().intent).toBe(intent);
  });

  it("selects a session on the selected node without touching the node", () => {
    const { store, route } = routeOwner({ nodeDid: "a", sessionId: null });
    route.followRoute("a-1");
    expect(store.getState()).toMatchObject({ nodeDid: "a", sessionId: "a-1" });
  });

  it("keeps a mailbox item's cause when it opens into a new session", () => {
    const { store, route } = routeOwner({ nodeDid: "a", sessionId: null });
    selection.openMailboxRoute(store, {
      itemId: "item-1",
      nodeDid: "a",
      agentId: "engineer",
      sessionId: null,
    });
    route.followRoute(null);
    expect(store.getState().mailboxRoute?.itemId).toBe("item-1");
  });

  it("starts a new session for an ordinary new-session route", () => {
    const { store, route } = routeOwner();
    route.followRoute(null);
    expect(store.getState()).toMatchObject({
      nodeDid: "a",
      sessionId: null,
      composingFor: "a",
    });
  });
});
