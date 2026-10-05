/* Placement is decided by what came before, never by what comes after:
   adding items to the end must never change the entries already made. */
import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import {
  groupLabel,
  groupTranscript,
  liveGroupLabel,
} from "@/screens/transcript-groups";

let seq = 0;
const tool = (
  kind: "fileRead" | "fileEdit" | "command" = "fileRead",
): RenderedToolCallView =>
  ({
    itemKey: `t${++seq}`,
    toolName: kind,
    statusKind: "success",
    presentation:
      kind === "fileRead"
        ? {
            kind,
            operation: "read",
            target: "a.rs",
            returnedCount: null,
            totalCount: null,
            truncated: false,
            body: "",
            fallbackOutput: null,
          }
        : kind === "fileEdit"
          ? {
              kind,
              operation: "replace",
              path: "a.rs",
              created: false,
              replacementsApplied: 1,
              diff: [],
              fallbackOutput: null,
            }
          : {
              kind,
              command: "ls",
              exitCode: 0,
              timedOut: false,
              failed: false,
              durationMs: 1,
              cwd: "~",
              executionMode: "sandboxed",
              networkMode: "disabled",
              stdout: "",
              stderr: "",
              fallbackOutput: null,
            },
  }) as unknown as RenderedToolCallView;

const group = (n: number, ...tools: RenderedToolCallView[]): RenderedTimelineItem => ({
  kind: "toolGroup",
  itemKey: `g${n}`,
  messageSequence: n,
  tools,
});
const said = (
  n: number,
  content: string | null,
  reasoning: string | null = null,
): RenderedTimelineItem => ({
  kind: "assistantMessage",
  itemKey: `a${n}`,
  sequence: n,
  content,
  reasoning,
  timestamp: null,
});
const person = (n: number): RenderedTimelineItem => ({
  kind: "userMessage",
  itemKey: `u${n}`,
  sequence: n,
  content: "go on",
  timestamp: null,
});
const live = (content: string | null): RenderedTimelineItem => ({
  kind: "liveAssistant",
  itemKey: "live",
  content,
  reasoning: null,
});

/* a turn the way the runtime sends it: narration, then one call per group */
const turn: RenderedTimelineItem[] = [
  person(1),
  said(2, "Looking at the export route."),
  group(2, tool()),
  said(3, null),
  group(3, tool()),
  said(4, null, "check the key"),
  group(4, tool("command")),
  said(5, "Now editing."),
  group(5, tool("fileEdit")),
  group(6, tool("fileEdit")),
  said(7, "Done: the cache is keyed by tenant."),
];

const shape = (entries: ReturnType<typeof groupTranscript>) =>
  entries.map((e) =>
    e.kind === "item" ? e.key : `${e.key}[${e.members.map((m) => m.key).join(",")}]`,
  );

describe("placing activity", () => {
  it("puts consecutive calls in one group, with thoughts between them", () => {
    const out = groupTranscript(turn);
    const groups = out.filter((e) => e.kind === "group");
    expect(groups).toHaveLength(2);
    if (groups[0]!.kind === "group") {
      expect(groups[0]!.key).toBe("g2");
      expect(groups[0]!.members.map((m) => m.kind)).toEqual([
        "tool",
        "tool",
        "thought",
        "tool",
      ]);
    }
  });

  it("ends a group at anything that says something", () => {
    const out = groupTranscript(turn);
    expect(out.map((e) => e.kind)).toEqual([
      "item",
      "item",
      "group",
      "item",
      "group",
      "item",
    ]);
  });

  it("settles a group once something ends it, and not before", () => {
    const out = groupTranscript(turn.slice(0, 7));
    const last = out.at(-1)!;
    expect(last.kind === "group" && last.settled).toBe(false);
    const first = groupTranscript(turn).find((e) => e.kind === "group")!;
    expect(first.kind === "group" && first.settled).toBe(true);
  });

  it("does not end a group at the live tail before it says anything", () => {
    const out = groupTranscript([...turn.slice(0, 5), live(null)]);
    const g = out.find((e) => e.kind === "group")!;
    expect(g.kind === "group" && g.settled).toBe(false);
  });

  it("keeps what was already placed when more arrives — every prefix", () => {
    for (let i = 1; i <= turn.length; i++) {
      const before = shape(groupTranscript(turn.slice(0, i)));
      const after = shape(groupTranscript(turn));
      /* each entry made from a prefix is still there, same key, and a group
         has only grown at its end */
      before.forEach((entry, j) => {
        const later = after[j]!;
        if (entry.includes("[")) {
          expect(later.split("[")[0]).toBe(entry.split("[")[0]);
          expect(later.slice(0, entry.length - 1)).toBe(entry.slice(0, -1));
        } else expect(later).toBe(entry);
      });
    }
  });
});

describe("naming a group", () => {
  it("says what it did, by kind", () => {
    const out = groupTranscript(turn);
    const g = out.find((e) => e.kind === "group")!;
    if (g.kind === "group")
      expect(groupLabel(g.members)).toBe("Read 2 files, ran 1 command");
  });

  it("keeps its words as the counts change", () => {
    expect(groupLabel([{ kind: "tool", key: "x", tool: tool("fileEdit") }])).toBe(
      "Edited 1 file",
    );
  });
});

describe("naming a live group", () => {
  const running = (kind: "fileRead" | "fileEdit" | "command") =>
    ({ ...tool(kind), statusKind: "running" }) as RenderedToolCallView;
  it("says what the newest call is doing while it runs", () => {
    expect(
      liveGroupLabel([{ kind: "tool", key: "x", tool: running("fileEdit") }]),
    ).toBe("Editing a.rs…");
    expect(liveGroupLabel([{ kind: "tool", key: "y", tool: running("command") }])).toBe(
      "Running ls…",
    );
  });
  it("says nothing between calls or once done, so the summary shows", () => {
    expect(
      liveGroupLabel([{ kind: "tool", key: "z", tool: tool("fileRead") }]),
    ).toBeNull();
  });
});

describe("the order of a burst", () => {
  /* keys the way the runtime writes them: the call's position at the end */
  const key = (i: number) =>
    `bae-c74c:provider:bae-c74c:{"kind":"provider_turn","scope":"inference.1","turn_index":1,"attempt":0}:${i}`;
  const burst = (order: number[]) =>
    ({
      kind: "toolGroup",
      itemKey: "g401",
      messageSequence: 401,
      tools: order.map((i) => ({ ...tool(), itemKey: key(i) })),
    }) as RenderedTimelineItem;

  it("puts calls in the order they ran, not in the order their keys sort as text", () => {
    /* what the query returns for fifteen calls: 10-14 between 1 and 2 */
    const asQueried = [0, 1, 10, 11, 12, 13, 14, 2, 3, 4, 5, 6, 7, 8, 9];
    const g = groupTranscript([burst(asQueried)])[0]!;
    if (g.kind !== "group") throw new Error("expected a group");
    expect(g.members.map((m) => Number(m.key.split(":").pop()))).toEqual([
      ...Array(15).keys(),
    ]);
  });

  it("adds a call arriving live at the end", () => {
    const before = groupTranscript([burst([0, 1, 2, 3, 4, 5, 6, 7, 8, 9])])[0]!;
    const after = groupTranscript([burst([0, 1, 10, 2, 3, 4, 5, 6, 7, 8, 9])])[0]!;
    if (before.kind !== "group" || after.kind !== "group")
      throw new Error("expected groups");
    expect(after.members.slice(0, 10).map((m) => m.key)).toEqual(
      before.members.map((m) => m.key),
    );
    expect(after.members[after.members.length - 1]!.key).toBe(key(10));
  });
});
