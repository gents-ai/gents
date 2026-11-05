import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import {
  greeting,
  groupItems,
  matches,
  orderItems,
  triageOf,
  waiting,
} from "@/screens/mailbox-triage";

const NOW = Date.parse("2026-09-25T12:00:00Z");
const min = (n: number) => new Date(NOW + n * 60_000).toISOString();

let n = 0;
const item = (kind: string, extra: Partial<MailboxItemView> = {}): MailboxItemView => ({
  itemId: `m${++n}`,
  itemKey: `mailbox/m${n}`,
  requesterDid: "did:me",
  nodeDid: "did:node",
  status: "open",
  kind,
  action: "ack",
  title: `${kind} ${n}`,
  summary: null,
  payload: null,
  sourceKind: "agent",
  sourceId: "src",
  sessionId: null,
  requestId: null,
  causeDocId: null,
  targetNodeDid: "did:node",
  targetAgentDid: "b1",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: min(-n),
  ...extra,
});

describe("triage", () => {
  it("puts what needs a decision first and what is done last", () => {
    const groups = groupItems([
      item("finished"),
      item("flag"),
      item("failed"),
      item("gate"),
      item("ask"),
    ]);
    expect(groups.map((g) => g.key)).toEqual(["decide", "wrong", "look", "done"]);
    expect(groups[0]!.items.map((m) => m.kind).sort()).toEqual(["ask", "gate"]);
  });

  it("leaves out empty groups", () => {
    expect(groupItems([item("flag")]).map((g) => g.key)).toEqual(["look"]);
  });

  it("lands a kind it does not know where a person will look", () => {
    expect(triageOf("surprise")).toBe("look");
  });
});

describe("order within a group", () => {
  it("the soonest deadline first, then no deadline, newest first", () => {
    const soon = item("ask", { deadlineAt: min(60) });
    const later = item("ask", { deadlineAt: min(240) });
    const older = item("ask", { createdAt: min(-500) });
    const newer = item("ask", { createdAt: min(-5) });
    const out = orderItems([older, later, newer, soon]);
    expect(out).toEqual([soon, later, newer, older]);
  });
});

describe("search", () => {
  it("reaches the payload and the source, and ignores case", () => {
    const m = item("gate", {
      title: "Sign the release note",
      payload: JSON.stringify({ build: "4021" }),
      sourceId: "graph-nightly",
    });
    expect(matches(m, "4021")).toBe(true);
    expect(matches(m, "RELEASE")).toBe(true);
    expect(matches(m, "nightly")).toBe(true);
    expect(matches(m, "rollback")).toBe(false);
    expect(matches(m, "   ")).toBe(true);
  });
});

describe("the mailbox's greeting", () => {
  it("follows the hour, and says hello late at night", () => {
    expect(greeting(5)).toBe("Good morning");
    expect(greeting(11)).toBe("Good morning");
    expect(greeting(12)).toBe("Good afternoon");
    expect(greeting(18)).toBe("Good evening");
    expect(greeting(23)).toBe("Hello");
    expect(greeting(2)).toBe("Hello");
  });

  it("says what waits, worst first, and leaves done out", () => {
    const groups = groupItems([
      item("ask"),
      item("gate"),
      item("flag"),
      item("failed"),
      item("finished"),
    ]);
    expect(waiting(groups)).toBe("1 went wrong · 2 to decide · 1 worth a look");
    expect(waiting(groupItems([item("ask")]))).toBe("1 to decide");
  });

  it("is all caught up when only finished work is left", () => {
    expect(waiting(groupItems([item("finished")]))).toBe("All caught up");
    expect(waiting([])).toBe("All caught up");
  });
});
