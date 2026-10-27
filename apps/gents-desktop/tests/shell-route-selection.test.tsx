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

describe("explicit node routing", () => {
  it("keeps a shared session on the node named by its route", () => {
    const stores = shellStores({
      deployments: [node("a", ["shared"]), node("b", ["shared"])],
      selection: { nodeDid: "a", sessionId: null },
    });
    const route = createSelectionActions({ stores });
    route.followRoute("shared", false, "b");
    expect(stores.selection.getState()).toMatchObject({
      nodeDid: "b",
      sessionId: "shared",
    });
    const intent = stores.selection.getState().intent;
    route.followRoute("shared", true, "b");
    expect(stores.selection.getState()).toMatchObject({
      nodeDid: "b",
      sessionId: "shared",
      intent,
    });
  });

  it("prefers the active node for an unqualified shared-session route", () => {
    const stores = shellStores({
      deployments: [node("a", ["shared"]), node("b", ["shared"])],
      selection: { nodeDid: "b", sessionId: null },
    });
    createSelectionActions({ stores }).followRoute("shared");
    expect(stores.selection.getState()).toMatchObject({
      nodeDid: "b",
      sessionId: "shared",
    });
  });

  it("retains the explicit destination while its session metadata is missing", () => {
    const { store, route } = routeOwner();
    route.followRoute("a-1", false, "not-loaded");
    expect(store.getState()).toMatchObject({ nodeDid: "not-loaded", sessionId: "a-1" });
    route.startNewSession();
    expect(store.getState()).toMatchObject({ nodeDid: "not-loaded", sessionId: "a-1" });
  });
});

it("keeps an already open remote session when only its local replica is listed", () => {
  const stores = shellStores({
    deployments: [node("a", ["shared"]), node("b", [])],
    selection: { nodeDid: "b", sessionId: "shared" },
  });
  const intent = stores.selection.getState().intent;
  createSelectionActions({ stores }).followRoute("shared", true);
  expect(stores.selection.getState()).toMatchObject({
    nodeDid: "b",
    sessionId: "shared",
    intent,
  });
});
