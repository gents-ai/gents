/* The mailbox's order: what needs a decision first, the soonest deadline
   first within a group, and a search that reaches into the payload. */
import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import {
  countKinds,
  groupItems,
  matches,
  orderItems,
  triageOf,
} from "@/screens/mailbox-triage";

const NOW = Date.parse("2026-09-25T12:00:00Z");
const min = (n: number) => new Date(NOW + n * 60_000).toISOString();

let n = 0;
const item = (kind: string, extra: Partial<MailboxItemView> = {}): MailboxItemView => ({
  itemId: `m${++n}`,
  itemKey: `mailbox/m${n}`,
  requesterDid: "did:me",
  agentDid: "did:agent",
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
  graphRunId: null,
  causeDocId: null,
  targetAgentDid: "did:agent",
  targetBehaviorId: "b1",
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

describe("kind counts", () => {
  it("keeps a fixed order and appends what it does not know", () => {
    const counts = countKinds([item("flag"), item("ask"), item("odd"), item("ask")]);
    expect(counts).toEqual([
      { kind: "ask", count: 2 },
      { kind: "flag", count: 1 },
      { kind: "odd", count: 1 },
    ]);
  });
});
