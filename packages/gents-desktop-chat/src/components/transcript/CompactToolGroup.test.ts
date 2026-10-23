import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { MessageList } from "../Transcript.js";
import { CompactToolGroup, coalesceToolGroups } from "./CompactToolGroup.js";

function tool(itemKey: string, statusKind = "success"): RenderedToolCallView {
  return {
    itemKey,
    statusKind,
    toolName: "read_file",
    requestTurn: {
      requestDocId: "physical-request",
      requestId: "request",
      settled: null,
    },
    reconstruction: { state: "ready" },
    presentation: {
      kind: "fileRead",
      operation: "read_file",
      target: "README.md",
      returnedCount: 1,
      totalCount: 1,
      truncated: false,
      body: "The complete file contents",
      fallbackOutput: null,
    },
  };
}

describe("compact action history", () => {
  it("keeps cancellation evidence behind a compact stop disclosure", () => {
    const html = renderToStaticMarkup(
      createElement(MessageList, {
        timelineItems: [],
        compactTools: true,
        requestCancelCause: {
          cause: "userCancelled",
          source: "requestInterrupt",
          confidence: "certain",
          at: null,
          evidence: ["direct interrupt"],
        },
      }),
    );
    expect(html).toContain(
      '<details class="turn-stop"><summary>Stopped by you</summary>',
    );
    expect(html).not.toContain('class="cause-badge');
    expect(html).toContain("direct interrupt");
  });
  it("combines consecutive groups without crossing a message or changing native items", () => {
    const group = (itemKey: string): RenderedTimelineItem => ({
      kind: "toolGroup",
      itemKey,
      messageSequence: 1,
      tools: [tool(itemKey)],
    });
    const items: RenderedTimelineItem[] = [
      group("first"),
      group("second"),
      {
        kind: "assistantMessage",
        itemKey: "explanation",
        content: "I found the cause.",
        reconstruction: { state: "ready" },
      },
      group("third"),
    ];
    const original = JSON.stringify(items);
    const html = renderToStaticMarkup(
      createElement(MessageList, { timelineItems: items, compactTools: true }),
    );
    expect(html.match(/class="tool-activity-row/g)).toHaveLength(2);
    expect(html).toContain("Explored files");
    expect(html).toContain("I found the cause.");
    expect(html).not.toContain("The complete file contents");
    expect(JSON.stringify(items)).toBe(original);
  });

  it("replaces the active action with a summary when its native status settles", () => {
    const html = (status: string) =>
      renderToStaticMarkup(
        createElement(CompactToolGroup, { tools: [tool("read", status)] }),
      );
    expect(html("running")).toContain("Reading README.md");
    expect(html("success")).toContain("Explored files");
    expect(html("error")).toContain("1 action failed");
    expect(html("unknown")).toContain("Action status unavailable");
    expect(html("unknown")).not.toContain("Explored");
  });

  it("does not expose an unavailable action payload in its summary", () => {
    const row = {
      ...tool("read", "running"),
      reconstruction: { state: "denied" as const },
    };
    const html = renderToStaticMarkup(
      createElement(CompactToolGroup, { tools: [row] }),
    );
    expect(html).toContain("Running Read file");
    expect(html).not.toContain("README.md");
    expect(html).not.toContain("The complete file contents");
  });

  it("keeps finished actions visible beside current work", () => {
    const html = renderToStaticMarkup(
      createElement(CompactToolGroup, {
        tools: [tool("past"), tool("current", "running")],
      }),
    );
    expect(html.match(/class="tool-activity-row/g)).toHaveLength(2);
    expect(html).toContain("Explored files");
    expect(html).toContain("Reading README.md");
  });

  it.each(["userCancelled", "interrupted"])(
    "keeps %s actions separate from failures",
    (cause) => {
      const stopped = {
        ...tool("stopped", "error"),
        cancelCause: {
          cause,
          source: "toolLifecycle",
          confidence: "certain",
          at: null,
          evidence: [],
        },
      };
      const html = renderToStaticMarkup(
        createElement(CompactToolGroup, {
          tools: [tool("failed", "error"), stopped, tool("current", "running")],
        }),
      );
      expect(html).toContain("1 action failed");
      expect(html).toContain("1 action stopped");
      expect(html).not.toContain("2 actions failed");
      expect(html).toContain("Reading README.md");
    },
  );
});

describe("compact action summaries", () => {
  it("summarizes mixed settled outcomes without losing failure or cancellation evidence", () => {
    const stopped = {
      ...tool("stopped", "error"),
      cancelCause: {
        cause: "userCancelled" as const,
        source: "toolLifecycle" as const,
        confidence: "certain" as const,
        at: null,
        evidence: [],
      },
    };
    const tools = [
      tool("read-a"),
      tool("failed", "error"),
      tool("read-b"),
      stopped,
      tool("unknown", "unknown"),
    ];
    const original = JSON.stringify(tools);
    const html = renderToStaticMarkup(
      createElement(CompactToolGroup, { tools }),
    );
    expect(html.match(/class="tool-activity-row/g)).toHaveLength(1);
    expect(html).toContain("Explored files");
    expect(html).toContain("1 action failed");
    expect(html).toContain("1 action stopped");
    expect(html).toContain("1 status unavailable");
    expect(html).toContain('class="tool-activity-errors"');
    expect(JSON.stringify(tools)).toBe(original);
  });

  it("bounds summary rows for many simultaneous actions", () => {
    const tools = [
      tool("past"),
      ...Array.from({ length: 500 }, (_, index) =>
        tool(`current-${index}`, "running"),
      ),
    ];
    const html = renderToStaticMarkup(
      createElement(CompactToolGroup, { tools }),
    );
    expect(html.match(/class="tool-activity-row/g)).toHaveLength(3);
    expect(html).toContain("Explored files");
    expect(html).toContain("499 actions running");
    expect(html).toContain("Reading README.md");
  });

  it("keeps mixed history beside current work instead of fragmenting each outcome", () => {
    const html = renderToStaticMarkup(
      createElement(CompactToolGroup, {
        tools: [
          tool("first"),
          tool("current", "running"),
          tool("failed", "error"),
          tool("last"),
        ],
      }),
    );
    expect(html.match(/class="tool-activity-row/g)).toHaveLength(2);
    expect(html).toContain("1 action failed");
    expect(html).toContain("Reading README.md");
  });

  it("retains narrative, user, automated and pending input boundaries", () => {
    const group = (key: string): RenderedTimelineItem => ({
      kind: "toolGroup",
      itemKey: key,
      messageSequence: 1,
      tools: [tool(key)],
    });
    const boundaries: RenderedTimelineItem[] = [
      {
        kind: "assistantMessage",
        itemKey: "explanation",
        content: "Explain the result",
        reconstruction: { state: "ready" },
      },
      {
        kind: "userMessage",
        itemKey: "follow-up",
        ownsTurn: true,
        content: "Continue",
        reconstruction: { state: "ready" },
      },
      {
        kind: "pendingUserTurn",
        itemKey: "queued",
        requestId: "queued",
        content: "Queued input",
        selectedSkillIds: [],
      },
      {
        kind: "liveAssistant",
        itemKey: "stream",
        content: "Current commentary",
      },
    ];
    for (const boundary of boundaries) {
      const before = group("before"),
        after = group("after");
      const result = coalesceToolGroups([before, boundary, after]);
      expect(result).toEqual([before, boundary, after]);
      expect(result[0]).toBe(before);
      expect(result[2]).toBe(after);
    }
  });

  it("coalesces a large run once while preserving ordered identities and source objects", () => {
    const groups: RenderedTimelineItem[] = Array.from(
      { length: 2000 },
      (_, index) => ({
        kind: "toolGroup",
        itemKey: `group-${index}`,
        messageSequence: index,
        tools: [tool(`tool-${index}`)],
      }),
    );
    const original = JSON.stringify(groups);
    const result = coalesceToolGroups(groups);
    expect(result).toHaveLength(1);
    const merged = result[0];
    expect(merged?.kind).toBe("toolGroup");
    if (merged?.kind !== "toolGroup")
      throw new Error("Expected merged tool group");
    expect(merged.itemKey).toBe("group-0");
    expect(merged.tools.map((tool) => tool.itemKey)).toEqual(
      Array.from({ length: 2000 }, (_, index) => `tool-${index}`),
    );
    expect(JSON.stringify(groups)).toBe(original);
  });
});

it("preserves a physical turn boundary even when logical request IDs repeat", () => {
  const first = tool("first");
  const second = tool("second");
  first.requestTurn = {
    requestDocId: "physical-first",
    requestId: "repeated",
    settled: true,
  };
  second.requestTurn = {
    requestDocId: "physical-second",
    requestId: "repeated",
    settled: false,
  };
  const groups: RenderedTimelineItem[] = [first, second].map(
    (entry, index) => ({
      kind: "toolGroup",
      itemKey: `group-${index}`,
      tools: [entry],
    }),
  );
  expect(coalesceToolGroups(groups)).toEqual(groups);
});

function boundaryGroup(
  itemKey: string,
  tools: RenderedToolCallView[],
): RenderedTimelineItem {
  return { kind: "toolGroup", itemKey, tools };
}

it("does not merge adjacent groups when physical ownership is unavailable", () => {
  const first = tool("unknown-first");
  const second = tool("unknown-second");
  delete first.requestTurn;
  delete second.requestTurn;
  const groups = [
    boundaryGroup("first", [first]),
    boundaryGroup("second", [second]),
  ];
  expect(coalesceToolGroups(groups)).toEqual(groups);
});

it("does not merge groups whose tools have different physical owners", () => {
  const mixed = (prefix: string) =>
    ["left", "right"].map((side) => {
      const entry = tool(`${prefix}-${side}`);
      entry.requestTurn = {
        requestDocId: `${prefix}-${side}`,
        requestId: "repeated",
        settled: null,
      };
      return entry;
    });
  const groups = [
    boundaryGroup("first", mixed("first")),
    boundaryGroup("second", mixed("second")),
  ];
  expect(coalesceToolGroups(groups)).toEqual(groups);
});

it("retains unchanged group objects across adjacent physical request boundaries", () => {
  const left = tool("left");
  const right = tool("right");
  right.requestTurn = {
    requestDocId: "another-request",
    requestId: "another",
    settled: null,
  };
  const first = boundaryGroup("first", [left]);
  const second = boundaryGroup("second", [right]);
  const result = coalesceToolGroups([first, second]);
  expect(result[0]).toBe(first);
  expect(result[1]).toBe(second);
});
