import { describe, expect, it } from "vitest";

import { createSelectionStore, selection } from "../src/hooks/selectionStore";

const route = {
  itemId: "item-1",
  agentDid: "node-a",
  behaviorId: "engineer",
  sessionId: "session-1",
};

describe("the selection store", () => {
  it("holds a mailbox route while the selection is the one it set up", () => {
    const store = createSelectionStore();
    selection.openMailboxRoute(store, route);
    expect(store.getState()).toMatchObject({
      agentDid: "node-a",
      behaviorId: "engineer",
      sessionId: "session-1",
      mailboxRoute: route,
    });
    expect(store.getState().composingFor).toBe("node-a");

    /* a snapshot settling the same behavior is not a move */
    selection.settleBehavior(store, "engineer");
    expect(store.getState().mailboxRoute).toEqual(route);
  });

  it.each([
    [
      "another behavior settles",
      (s: ReturnType<typeof createSelectionStore>) =>
        selection.settleBehavior(s, "writer"),
    ],
    [
      "the person picks a behavior",
      (s: ReturnType<typeof createSelectionStore>) =>
        selection.selectBehavior(s, "writer"),
    ],
    [
      "the person picks a node",
      (s: ReturnType<typeof createSelectionStore>) =>
        selection.selectAgent(s, "node-b"),
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
    const store = createSelectionStore({ agentDid: "node-a" });
    const captured = selection.captureIntent(store);
    selection.settleBehavior(store, "engineer");
    expect(selection.acceptsIntent(store, captured)).toBe(true);
    selection.selectSession(store, "session-1");
    expect(selection.acceptsIntent(store, captured)).toBe(false);
  });

  it("selecting the same node again keeps its session", () => {
    const store = createSelectionStore({ agentDid: "node-a", sessionId: "session-1" });
    expect(selection.selectAgent(store, "node-a")).toBe(false);
    expect(store.getState().sessionId).toBe("session-1");
    expect(selection.selectAgent(store, "node-b")).toBe(true);
    expect(store.getState()).toMatchObject({ sessionId: null, behaviorId: null });
  });
});
