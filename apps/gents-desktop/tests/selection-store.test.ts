import { describe, expect, it } from "vitest";

import { createSelectionStore, selection } from "../src/hooks/selectionStore";

const route = {
  itemId: "item-1",
  nodeDid: "node-a",
  agentId: "engineer",
  sessionId: "session-1",
};

describe("the selection store", () => {
  it("holds a mailbox route while the selection is the one it set up", () => {
    const store = createSelectionStore();
    selection.openMailboxRoute(store, route);
    expect(store.getState()).toMatchObject({
      nodeDid: "node-a",
      agentId: "engineer",
      sessionId: "session-1",
      mailboxRoute: route,
    });
    expect(store.getState().composingFor).toBe("node-a");

    /* a snapshot settling the same agent is not a move */
    selection.settleAgent(store, "engineer");
    expect(store.getState().mailboxRoute).toEqual(route);
  });

  it.each([
    [
      "another agent settles",
      (s: ReturnType<typeof createSelectionStore>) =>
        selection.settleAgent(s, "writer"),
    ],
    [
      "the person picks an agent",
      (s: ReturnType<typeof createSelectionStore>) =>
        selection.selectAgent(s, "writer"),
    ],
    [
      "the person picks a node",
      (s: ReturnType<typeof createSelectionStore>) => selection.selectNode(s, "node-b"),
    ],
    [
      "a retried request moves session",
      (s: ReturnType<typeof createSelectionStore>) =>
        selection.settleSession(s, "session-2"),
    ],
  ])("lets go of a mailbox route when %s", (_, move) => {
    const store = createSelectionStore();
    selection.openMailboxRoute(store, route);
    move(store);
    expect(store.getState().mailboxRoute).toBeNull();
    expect(store.getState().composingFor).toBeNull();
  });

  it("advances the intent on navigation, so an older async result is dropped", () => {
    const store = createSelectionStore({ nodeDid: "node-a" });
    const captured = selection.captureIntent(store);
    selection.settleAgent(store, "engineer");
    expect(selection.acceptsIntent(store, captured)).toBe(true);
    selection.selectSession(store, "session-1");
    expect(selection.acceptsIntent(store, captured)).toBe(false);
  });

  it("selecting the same node again keeps its session", () => {
    const store = createSelectionStore({ nodeDid: "node-a", sessionId: "session-1" });
    expect(selection.selectNode(store, "node-a")).toBe(false);
    expect(store.getState().sessionId).toBe("session-1");
    expect(selection.selectNode(store, "node-b")).toBe(true);
    expect(store.getState()).toMatchObject({ sessionId: null, agentId: null });
  });
});
