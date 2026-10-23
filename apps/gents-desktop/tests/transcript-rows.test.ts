import type {
  RenderedTimelineItem,
  SessionRowsView,
  SessionRowsWindow,
} from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import {
  applyRowsRead,
  drawnRows,
  noRows,
  type HeldRows,
} from "../src/ui/lib/transcriptRows";

const user = (sequence: number): RenderedTimelineItem =>
  ({
    kind: "userMessage",
    itemKey: `u${sequence}`,
    sequence,
    content: `question ${sequence}`,
  }) as RenderedTimelineItem;
const reply = (sequence: number): RenderedTimelineItem =>
  ({
    kind: "assistantMessage",
    itemKey: `a${sequence}`,
    sequence,
    content: `answer ${sequence}`,
  }) as RenderedTimelineItem;
const tools = (sequence: number): RenderedTimelineItem =>
  ({
    kind: "toolGroup",
    itemKey: `tools-${sequence}`,
    messageSequence: sequence,
    tools: [],
  }) as RenderedTimelineItem;
const pending: RenderedTimelineItem = {
  kind: "pendingUserTurn",
  itemKey: "pending-r9",
} as RenderedTimelineItem;
const live: RenderedTimelineItem = {
  kind: "liveAssistant",
  itemKey: "live-assistant-r9",
  content: "typing",
} as RenderedTimelineItem;

const read = (
  rows: RenderedTimelineItem[],
  coveredFrom: number,
  coveredThrough: number | null,
  more: Partial<SessionRowsView> = {},
): SessionRowsView => ({
  sessionId: "s",
  nodeDid: "did:test:node",
  rows,
  coveredFrom,
  coveredThrough,
  hasOlder: coveredFrom > 0,
  hasNewer: coveredThrough !== null,
  complete: true,
  unreadable: null,
  queryCount: 1,
  ...more,
});
const keys = (items: RenderedTimelineItem[]) => items.map((item) => item.itemKey);
const apply = (held: HeldRows, window: SessionRowsWindow, view: SessionRowsView) =>
  applyRowsRead(held, window, view);

describe("rows held beyond the newest page", () => {
  it("keeps context events after the message and tool group they are anchored to", () => {
    const compaction = {
      kind: "sessionCompaction",
      itemKey: "session-compaction:c1",
      sequence: 11,
    } as RenderedTimelineItem;
    const reduction = {
      kind: "requestContextReductionSummary",
      itemKey: "request-context-reduction:r1",
      sequence: 11,
    } as RenderedTimelineItem;
    const held = apply(
      noRows("s"),
      { kind: "before", sequence: 20 },
      read([reduction, compaction, tools(11), reply(11), user(12)], 11, 19),
    );
    expect(keys(held.rows)).toEqual([
      "a11",
      "tools-11",
      "session-compaction:c1",
      "request-context-reduction:r1",
      "u12",
    ]);
  });

  it("draws held history before the newest page, which keeps its own order", () => {
    const held = apply(
      noRows("s"),
      { kind: "before", sequence: 20 },
      read([user(10), tools(11), reply(11), user(12)], 10, 19),
    );
    /* a message comes before the tool group that rides with it */
    expect(keys(held.rows)).toEqual(["u10", "a11", "tools-11", "u12"]);
    const drawn = drawnRows(held, [user(20), pending, reply(21), live], true);
    expect(drawn.joined).toBe(true);
    expect(keys(drawn.items)).toEqual([
      "u10",
      "a11",
      "tools-11",
      "u12",
      "u20",
      "pending-r9",
      "a21",
      "live-assistant-r9",
    ]);
    expect(drawn.olderBefore).toBe(10);
  });

  it("asks the newest page whether older history exists before any read", () => {
    expect(drawnRows(noRows("s"), [user(20)], true).olderBefore).toBe(20);
    expect(drawnRows(noRows("s"), [user(1)], false).olderBefore).toBeNull();
  });

  it("stops at the start of the session", () => {
    const held = apply(
      noRows("s"),
      { kind: "before", sequence: 5 },
      read([user(1)], 0, 4, {
        hasOlder: false,
      }),
    );
    expect(drawnRows(held, [user(5)], true).olderBefore).toBeNull();
  });

  it("does not draw a held row twice when the newest page has it too", () => {
    const held = apply(
      noRows("s"),
      { kind: "before", sequence: 21 },
      read([user(19), reply(20)], 19, 20),
    );
    const drawn = drawnRows(held, [reply(20), user(21)], true);
    expect(keys(drawn.items)).toEqual(["u19", "a20", "u21"]);
  });

  it("draws a jump alone until the reads between it and the newest page close the gap", () => {
    let held = apply(
      noRows("s"),
      { kind: "around", sequence: 300 },
      read([user(290), reply(291), user(300), reply(301)], 280, 319),
    );
    let drawn = drawnRows(held, [user(900), reply(901)], true);
    expect(drawn.joined).toBe(false);
    expect(keys(drawn.items)).toEqual(["u290", "a291", "u300", "a301"]);
    expect(drawn.newerAfter).toBe(319);
    expect(drawn.olderBefore).toBe(280);

    held = apply(held, { kind: "after", sequence: 319 }, read([user(320)], 320, 899));
    drawn = drawnRows(held, [user(900), reply(901)], true);
    expect(drawn.joined).toBe(true);
    expect(keys(drawn.items)).toEqual([
      "u290",
      "a291",
      "u300",
      "a301",
      "u320",
      "u900",
      "a901",
    ]);
    expect(drawn.newerAfter).toBeNull();
  });

  it("joins once a newer read reaches the newest message", () => {
    let held = apply(
      noRows("s"),
      { kind: "around", sequence: 50 },
      read([user(50)], 30, 69),
    );
    held = apply(held, { kind: "after", sequence: 69 }, read([user(70)], 70, null));
    expect(drawnRows(held, [user(70), reply(71)], true).joined).toBe(true);
  });

  it("is not complete once any read came from a copy still arriving", () => {
    let held = apply(
      noRows("s"),
      { kind: "before", sequence: 40 },
      read([user(30)], 30, 39),
    );
    expect(held.complete).toBe(true);
    held = apply(
      held,
      { kind: "before", sequence: 30 },
      read([user(20)], 20, 29, {
        complete: false,
      }),
    );
    expect(held.complete).toBe(false);
    /* a jump starts over */
    held = apply(held, { kind: "around", sequence: 5 }, read([user(5)], 0, 24));
    expect(held.complete).toBe(true);
  });

  describe("one row per message across held history and the newest page", () => {
    const pendingFor = (requestId: string, more: Partial<RenderedTimelineItem> = {}) =>
      ({
        kind: "pendingUserTurn",
        itemKey: `pending-${requestId}`,
        requestId,
        content: "same text",
        selectedSkillIds: [],
        lifecycleState: "pending",
        foldedIntoRequestId: null,
        origin: null,
        createdAt: null,
        ...more,
      }) as RenderedTimelineItem;
    const heldWith = (row: RenderedTimelineItem) =>
      apply(noRows("s"), { kind: "before", sequence: 20 }, read([row], 10, 19));

    it("drops a pending turn whose saved message came in with older history", () => {
      const saved = {
        kind: "userMessage",
        itemKey: "authored-r",
        requestId: "r",
        ownsTurn: true,
        inputRequestId: "r",
        sequence: 12,
        content: "same text",
      } as RenderedTimelineItem;
      const drawn = drawnRows(heldWith(saved), [user(20), pendingFor("r")], true);
      expect(keys(drawn.items)).toEqual(["authored-r", "u20"]);
    });

    it("drops a pending input folded into an earlier turn once its entry is held", () => {
      const folded = {
        kind: "userMessage",
        itemKey: "authored:doc-head:folded:doc-folded",
        requestId: "doc-head",
        inputRequestId: "folded",
        ownsTurn: false,
        sequence: 13,
        content: "also this",
      } as RenderedTimelineItem;
      const drawn = drawnRows(
        heldWith(folded),
        [
          pendingFor("folded", {
            lifecycleState: "superseded",
            foldedIntoRequestId: "head",
          }),
          user(20),
        ],
        true,
      );
      expect(keys(drawn.items)).toEqual(["authored:doc-head:folded:doc-folded", "u20"]);
    });

    it("drops a pending trigger input once its published item is held", () => {
      const origin = { kind: "trigger", triggerId: "nightly", triggerKind: "schedule" };
      const published = {
        kind: "automatedInput",
        itemKey: "authored:doc-t:prompt",
        requestId: "doc-t",
        inputRequestId: "t",
        sequence: 14,
        origin,
        content: "write the report",
      } as unknown as RenderedTimelineItem;
      const drawn = drawnRows(
        heldWith(published),
        [pendingFor("t", { origin } as Partial<RenderedTimelineItem>), user(20)],
        true,
      );
      expect(keys(drawn.items)).toEqual(["authored:doc-t:prompt", "u20"]);
    });

    it("keeps a pending turn when only its request's context row is held", () => {
      const context = {
        kind: "userMessage",
        itemKey: "authored-r-context",
        requestId: "r",
        ownsTurn: false,
        sequence: 15,
        content: "<context>\nworkspace instructions\n</context>",
      } as RenderedTimelineItem;
      const drawn = drawnRows(heldWith(context), [pendingFor("r"), user(20)], true);
      expect(keys(drawn.items)).toEqual(["authored-r-context", "pending-r", "u20"]);
    });

    it("keeps another request's pending turn with identical text", () => {
      const other = {
        kind: "userMessage",
        itemKey: "authored-r1",
        requestId: "r1",
        ownsTurn: true,
        sequence: 11,
        content: "same text",
      } as RenderedTimelineItem;
      const drawn = drawnRows(heldWith(other), [user(20), pendingFor("r2")], true);
      expect(keys(drawn.items)).toEqual(["authored-r1", "u20", "pending-r2"]);
    });
  });

  it("ignores a read for another session", () => {
    const held = noRows("s");
    expect(
      apply(held, { kind: "tip" }, { ...read([user(1)], 0, null), sessionId: "other" }),
    ).toBe(held);
  });
});
