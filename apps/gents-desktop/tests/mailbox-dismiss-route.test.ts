import { describe, expect, it, vi } from "vitest";

import { selection } from "../src/hooks/selectionStore";
import { testApp } from "./app-fixture";

/* a compose route opened on an item, with the selection it set up */
const routed = (itemId: string) => ({
  nodeDid: "did:key:a",
  agentId: "engineer",
  sessionId: null,
  mailboxRoute: {
    itemId,
    nodeDid: "did:key:a",
    agentId: "engineer",
    sessionId: null,
  },
});

describe("dismissing a mailbox item", () => {
  it("lets go of a compose route opened on that item", async () => {
    const app = testApp({
      api: { dismissMailboxItem: vi.fn().mockResolvedValue(undefined) },
      selection: routed("item-a"),
    });
    await app.actions.dismissMailboxItem("item-a");
    expect(app.stores.selection.getState().mailboxRoute).toBeNull();
  });

  it("keeps a route opened on another item while the dismissal was in flight", async () => {
    let finish = () => {};
    const app = testApp({
      api: {
        dismissMailboxItem: vi.fn(
          () => new Promise<void>((resolve) => (finish = resolve)),
        ),
      },
      selection: routed("item-a"),
    });
    const dismissal = app.actions.dismissMailboxItem("item-a");
    selection.openMailboxRoute(app.stores.selection, routed("item-b").mailboxRoute);
    finish();
    await dismissal;
    expect(app.stores.selection.getState().mailboxRoute?.itemId).toBe("item-b");
  });
});
