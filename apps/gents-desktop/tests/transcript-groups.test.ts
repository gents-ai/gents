import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import {
  completionOf,
  groupLabel,
  groupTranscript,
  isWakePrompt,
  liveGroupLabel,
} from "@/screens/transcript-groups";
import { assistantMessage, liveAssistant, userMessage } from "./timeline-fixture";

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
): RenderedTimelineItem =>
  assistantMessage({
    kind: "assistantMessage",
    itemKey: `a${n}`,
    sequence: n,
    content,
    reasoning,
    timestamp: null,
  });
const person = (n: number): RenderedTimelineItem =>
  userMessage({
    kind: "userMessage",
    itemKey: `u${n}`,
    sequence: n,
    content: "go on",
    timestamp: null,
  });
const live = (content: string | null): RenderedTimelineItem =>
  liveAssistant({
    kind: "liveAssistant",
    itemKey: "live",
    content,
    reasoning: null,
  });

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

  it("says how many calls failed, after what it did", () => {
    const failed = (t: RenderedToolCallView) =>
      ({ ...t, statusKind: "error" }) as RenderedToolCallView;
    const exited = (t: RenderedToolCallView) =>
      ({
        ...t,
        presentation: { ...t.presentation, exitCode: 1, failed: true },
      }) as RenderedToolCallView;
    expect(
      groupLabel([
        { kind: "tool", key: "a", tool: tool("command") },
        { kind: "tool", key: "b", tool: exited(tool("command")) },
        { kind: "tool", key: "c", tool: failed(tool("fileRead")) },
      ]),
    ).toBe("Read 1 file, ran 2 commands, 2 failed");
    expect(groupLabel([{ kind: "tool", key: "d", tool: tool("command") }])).toBe(
      "Ran 1 command",
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

describe("background results", () => {
  const completion = (id: string) =>
    `<tool-completion tool_call_id="${id}" tool_name="bash" status="completed">\n  <result>ok</result>\n</tool-completion>`;
  const result = (n: number, content: string): RenderedTimelineItem => ({
    kind: "automatedInput",
    itemKey: `b${n}`,
    sequence: n,
    origin: {
      kind: "backgroundCompletion",
      notification: !content
        .trim()
        .startsWith("Review the new background completion results"),
    },
    content,
    timestamp: null,
    reconstruction: { state: "ready" },
  });
  const background = (entries: ReturnType<typeof groupTranscript>) =>
    entries.map((e) =>
      e.kind === "item" && e.background
        ? `${e.key}{${e.background.map((r) => r.item.itemKey).join(",")}}`
        : e.key,
    );

  const WAKE =
    "Review the new background completion results and continue the task if needed.";
  const prompts = (entries: ReturnType<typeof groupTranscript>) =>
    entries.flatMap((e) =>
      e.kind === "item" && e.background
        ? e.background.map((r) => r.prompts.map((p) => p.itemKey).join(","))
        : [],
    );

  it("draws results with nothing between them as one row, under the first one's key", () => {
    const items = [
      person(1),
      result(2, completion("x")),
      result(3, "an input that names no call"),
      said(4, "Both finished."),
      result(5, completion("y")),
    ];
    expect(background(groupTranscript(items))).toEqual([
      "u1",
      "b2{b2,b3}",
      "a4",
      "b5{b5}",
    ]);
    for (let i = 1; i <= items.length; i++)
      background(groupTranscript(items.slice(0, i))).forEach((key, j) =>
        expect(background(groupTranscript(items))[j]!.split("{")[0]).toBe(
          key.split("{")[0],
        ),
      );
  });

  it("reads the wake prompt with the result it follows, not as a result", () => {
    const items = [
      person(1),
      result(2, completion("x")),
      result(3, completion("y")),
      result(4, WAKE),
      said(5, "Both finished."),
      result(6, completion("z")),
      result(7, `  ${WAKE}\n`),
    ];
    const out = groupTranscript(items);
    expect(background(out)).toEqual(["u1", "b2{b2,b3}", "a5", "b6{b6}"]);
    expect(prompts(out)).toEqual(["", "b4", "b7"]);
    for (let i = 1; i <= items.length; i++)
      background(groupTranscript(items.slice(0, i))).forEach((key, j) =>
        expect(background(out)[j]!.split("{")[0]).toBe(key.split("{")[0]),
      );
  });

  it("draws a wake prompt with no results before it as its own row", () => {
    const out = groupTranscript([
      said(1, "Waiting."),
      result(2, WAKE),
      result(3, completion("x")),
    ]);
    expect(background(out)).toEqual(["a1", "b2", "b3{b3}"]);
  });

  it("knows the wake prompt only by the runtime's exact words", () => {
    expect(isWakePrompt(WAKE)).toBe(true);
    expect(isWakePrompt(`${WAKE} Also do this.`)).toBe(false);
    expect(isWakePrompt("Review the new background completion results.")).toBe(false);
    expect(isWakePrompt(null)).toBe(false);
  });

  it("ends an open group and is ended by the next one", () => {
    const run = tool("command");
    const out = groupTranscript([
      group(1, run),
      result(2, completion("x")),
      group(3, tool()),
    ]);
    expect(out.map((e) => e.kind)).toEqual(["group", "item", "group"]);
    expect(out[0]!.kind === "group" && out[0]!.settled).toBe(true);
  });

  it("finds the call a result reports on among the drawn calls", () => {
    const call = { ...tool("command"), toolCallId: "call-1" };
    const out = groupTranscript([
      group(1, call),
      result(2, completion("call-1")),
      result(3, completion("gone")),
    ]);
    const entry = out[1]!;
    expect(entry.kind === "item" && entry.background?.map((r) => r.call)).toEqual([
      call,
      null,
    ]);
  });

  it("reads the call from the runtime's wrapper, and nothing else", () => {
    expect(
      completionOf(
        '<tool-completion tool_call_id="a&quot;b" tool_name="bash" status="failed">\n</tool-completion>',
      ),
    ).toEqual({ toolCallId: 'a"b', toolName: "bash", status: "failed" });
    expect(completionOf('quoting <tool-completion tool_call_id="x"')).toBeNull();
    expect(completionOf(null)).toBeNull();
  });
});
