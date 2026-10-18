import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { MessageList } from "../Transcript.js";
import { CompactToolGroup } from "./CompactToolGroup.js";

function tool(itemKey: string, statusKind = "success"): RenderedToolCallView {
  return {
    itemKey,
    statusKind,
    toolName: "read_file",
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
