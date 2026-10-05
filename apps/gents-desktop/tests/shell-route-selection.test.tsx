import { describe, expect, it, vi } from "vitest";

import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { createDesktopShellSelectionActions } from "../src/hooks/desktopShellSelectionActions";
import { selection } from "../src/hooks/selectionStore";
import { shellStores } from "./shell-fixture";

const node = (agentDid: string, sessionIds: string[]) =>
  ({
    agentDid,
    agentPrincipal: { defaultBehaviorId: null },
    behaviors: [],
    sessions: sessionIds.map((sessionId) => ({ agentDid, sessionId })),
    mailboxItems: [],
  }) as unknown as DeploymentView;

/* the route's owner over nodes a (session a-1) and b (session b-1) */
function routeOwner(initial = { agentDid: "a", sessionId: "a-1" as string | null }) {
  const stores = shellStores({
    deployments: [node("a", ["a-1"]), node("b", ["b-1"])],
    selection: initial,
  });
  const route = createDesktopShellSelectionActions({ stores, setError: vi.fn() });
  return { store: stores.selection, route };
}

describe("following the route", () => {
  it("selects a session on another node together with its node", () => {
    const { store, route } = routeOwner();
    route.followRoute("b-1");
    expect(store.getState()).toMatchObject({ agentDid: "b", sessionId: "b-1" });
    /* a snapshot landing afterwards changes nothing */
    const intent = store.getState().intent;
    route.followRoute("b-1", true);
    expect(store.getState().intent).toBe(intent);
  });

  it("selects a session on the selected node without touching the node", () => {
    const { store, route } = routeOwner({ agentDid: "a", sessionId: null });
    route.followRoute("a-1");
    expect(store.getState()).toMatchObject({ agentDid: "a", sessionId: "a-1" });
  });

  it("keeps a mailbox item's cause when it opens into a new session", () => {
    const { store, route } = routeOwner({ agentDid: "a", sessionId: null });
    selection.openMailboxRoute(store, {
      itemId: "item-1",
      agentDid: "a",
      behaviorId: "engineer",
      sessionId: null,
    });
    route.followRoute(null);
    expect(store.getState().mailboxRoute?.itemId).toBe("item-1");
  });

  it("starts a new session for an ordinary new-session route", () => {
    const { store, route } = routeOwner();
    route.followRoute(null);
    expect(store.getState()).toMatchObject({
      agentDid: "a",
      sessionId: null,
      composingFor: "a",
    });
  });
});
