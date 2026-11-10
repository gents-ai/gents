import { describe, expect, it } from "vitest";

import { isNewItem, restoreMailboxSeen } from "../src/ui/app/mailboxSeen";

const item = (
  over: { nodeDid?: string; itemId?: string; createdAt?: string } = {},
) => ({
  nodeDid: "did:key:a",
  itemId: "item",
  createdAt: "2026-10-31T12:00:00Z",
  ...over,
});

describe("which mailbox items are new here", () => {
  it("counts an item that came after this device started keeping track, until it is opened", () => {
    const since = "2026-10-31T11:00:00Z";
    expect(isNewItem(item(), { opened: [], since })).toBe(true);
    expect(isNewItem(item(), { opened: ["did:key:a:item"], since })).toBe(false);
  });

  it("counts nothing from before this device kept track, or before it started", () => {
    expect(isNewItem(item(), { opened: [], since: "2026-10-31T13:00:00Z" })).toBe(
      false,
    );
    expect(isNewItem(item(), { opened: [], since: null })).toBe(false);
  });

  it("keeps an opened item to its node, since item ids are a node's own", () => {
    const seen = { opened: ["did:key:a:item"], since: "2026-10-31T11:00:00Z" };
    expect(isNewItem(item({ nodeDid: "did:key:b" }), seen)).toBe(true);
  });

  it("restores only well-formed marks", () => {
    expect(
      restoreMailboxSeen({ opened: ["did:key:a:item", 7, null], since: 12 }),
    ).toEqual({ opened: ["did:key:a:item"], since: null });
    expect(restoreMailboxSeen("broken")).toEqual({ opened: [], since: null });
  });
});
