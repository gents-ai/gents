import { describe, expect, it } from "vitest";
import type {
  PendingQueueEntryView,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";
import {
  moveEdit,
  moveRun,
  pendingRows,
  removeEdit,
  rewriteEdit,
  separatedBySettings,
  steerEdit,
  steerRun,
} from "../../../packages/gents-desktop-client/src/pendingQueue";

const entry = (id: string, group: string | null = "same"): PendingQueueEntryView => ({
  requestDocId: `doc-${id}`,
  requestId: id,
  content: `message ${id}`,
  editable: group !== null,
  steerable: group !== null,
  editGroup: group,
});
const turn = (
  id: string,
  origin: PendingTurnView["origin"] = null,
): PendingTurnView => ({
  requestId: id,
  content: `message ${id}`,
  selectedSkillIds: [],
  lifecycleState: "pending",
  foldedIntoRequestId: null,
  origin,
  createdAt: null,
});

describe("pendingRows", () => {
  it("preserves native arrival order while locating controls in the pending queue", () => {
    const automated = turn("auto", {
      kind: "backgroundCompletion",
    } as PendingTurnView["origin"]);
    const rows = pendingRows(
      [turn("a"), automated, turn("b")],
      [entry("b"), entry("a")],
    );
    expect(rows.map((row) => [row.kind, row.turn.requestId])).toEqual([
      ["message", "a"],
      ["automated", "auto"],
      ["message", "b"],
    ]);
    expect(rows.map((row) => (row.kind === "message" ? row.index : null))).toEqual([
      1,
      null,
      0,
    ]);
  });

  it("offers editing only for entries the runtime lists as editable", () => {
    const rows = pendingRows(
      [turn("a"), turn("b"), turn("c")],
      [entry("a"), entry("b", null)],
    );
    expect(
      rows.map((row) => (row.kind === "message" ? row.entry?.requestId : null)),
    ).toEqual(["a", undefined, undefined]);
    expect(rows.map((row) => (row.kind === "message" ? row.index : null))).toEqual([
      0, -1, -1,
    ]);
  });

  it("lists every queued turn as it stands without a queue", () => {
    expect(pendingRows([turn("a")], null)).toEqual([
      { kind: "message", turn: turn("a"), entry: null, index: -1 },
    ]);
  });
});

describe("moveRun", () => {
  const queue = [entry("a"), entry("b"), entry("c", "other"), entry("d", null)];

  it("is the contiguous run a move crosses", () => {
    expect(moveRun(queue, 0, 1)).toEqual([queue[0], queue[1]]);
    expect(moveRun(queue, 1, 0)).toEqual([queue[0], queue[1]]);
  });

  it("refuses to cross another group, an entry it may not edit, or nothing", () => {
    expect(moveRun(queue, 1, 2)).toBeNull();
    expect(moveRun(queue, 2, 3)).toBeNull();
    expect(moveRun(queue, 1, 1)).toBeNull();
  });
});

describe("queue edits", () => {
  const queue = [entry("a"), entry("b"), entry("c")];

  it("moves within the run it crosses, fenced on the whole queue", () => {
    expect(moveEdit(queue, 2, 0)).toEqual({
      expectedRequestDocIds: ["doc-a", "doc-b", "doc-c"],
      selectedRequestDocIds: ["doc-a", "doc-b", "doc-c"],
      messages: [
        { requestDocId: "doc-c", content: "message c" },
        { requestDocId: "doc-a", content: "message a" },
        { requestDocId: "doc-b", content: "message b" },
      ],
    });
    expect(moveEdit(queue, 1, 1)).toBeNull();
  });

  it("rewrites and removes one entry, fenced on the whole queue", () => {
    expect(rewriteEdit(queue, queue[1]!, "new text")).toEqual({
      expectedRequestDocIds: ["doc-a", "doc-b", "doc-c"],
      selectedRequestDocIds: ["doc-b"],
      messages: [{ requestDocId: "doc-b", content: "new text" }],
    });
    expect(removeEdit(queue, queue[2]!)).toEqual({
      expectedRequestDocIds: ["doc-a", "doc-b", "doc-c"],
      selectedRequestDocIds: ["doc-c"],
      messages: [],
    });
  });

  it("sends the complete authored replacement with the selected queue slot", () => {
    const authored = {
      selected_skill_ids: ["review"],
      captured_context: [],
      attachments: [{ name: "note.txt", mediaType: "text/plain", data: "aGk=" }],
    };
    expect(rewriteEdit(queue, queue[1]!, "changed", authored)).toEqual({
      expectedRequestDocIds: ["doc-a", "doc-b", "doc-c"],
      selectedRequestDocIds: ["doc-b"],
      messages: [
        { requestDocId: "doc-b", content: "changed", authoredInput: authored },
      ],
    });
  });
});

describe("separatedBySettings", () => {
  it("is only two of a person's messages the runtime groups apart", () => {
    const queue = [entry("a"), entry("b", "other"), entry("c", null)];
    expect(separatedBySettings(queue, 0, 1)).toBe(true);
    expect(separatedBySettings(queue, 1, 2)).toBe(false);
  });
});

describe("steerRun", () => {
  it("takes every message queued before it, all steerable and under its settings", () => {
    const queue = [entry("a"), entry("b"), entry("c")];
    expect(steerRun(queue, 0)).toEqual([queue[0]]);
    expect(steerRun(queue, 2)).toEqual(queue);
  });

  it("refuses a message behind another group or an entry it may not steer", () => {
    const queue = [entry("a", "other"), entry("b"), entry("c", null), entry("d")];
    expect(steerRun(queue, 1)).toBeNull();
    expect(steerRun(queue, 2)).toBeNull();
    expect(steerRun(queue, 3)).toBeNull();
    expect(steerRun(queue, 4)).toBeNull();
  });

  it("names the message, fenced on the whole queue", () => {
    const queue = [entry("a"), entry("b")];
    expect(steerEdit(queue, queue[1]!)).toEqual({
      expectedRequestDocIds: ["doc-a", "doc-b"],
      requestDocId: "doc-b",
    });
  });
});
