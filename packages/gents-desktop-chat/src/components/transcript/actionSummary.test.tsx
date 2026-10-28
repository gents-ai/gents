import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import {
  actionCategory,
  completedActionLabel,
  representativeActionCategory,
  type ActionCategory,
} from "./actionSummary.js";
import { CompactToolGroup } from "./CompactToolGroup.js";
import { actionEntries } from "./actionPresentation.js";

function tool(
  category: ActionCategory,
  key: string = category,
): RenderedToolCallView {
  const base = {
    itemKey: key,
    requestId: "request",
    toolCallId: key,
    toolName: "untrusted display label",
    statusKind: "success",
    reconstruction: { state: "ready" as const },
  };
  if (category === "read" || category === "explore" || category === "search")
    return {
      ...base,
      presentation: {
        kind: "fileRead",
        operation:
          category === "read"
            ? "read_file"
            : category === "search"
              ? "grep"
              : "list_files",
        target: "/workspace/a",
        returnedCount: 1,
        totalCount: 1,
        truncated: false,
        body: "retained output",
        fallbackOutput: null,
      },
    };
  if (category === "edit")
    return {
      ...base,
      presentation: {
        kind: "fileEdit",
        operation: "edit_file",
        path: "/workspace/a",
        applied: null,
        created: null,
        replacementsApplied: null,
        diff: [],
        fallbackOutput: null,
      },
    };
  if (category === "command")
    return {
      ...base,
      presentation: {
        kind: "command",
        command: "untrusted display label",
        exitCode: null,
        timedOut: false,
        failed: false,
        durationMs: null,
        cwd: null,
        executionMode: null,
        networkMode: null,
        stdout: "",
        stderr: "",
        fallbackOutput: null,
      },
    };
  return {
    ...base,
    presentation: {
      kind: "generic",
      summary: "edited files",
      input: null,
      output: null,
    },
  };
}

const cases: [ActionCategory, string][] = [
  ["read", "file read"],
  ["explore", "exploration"],
  ["search", "search"],
  ["edit", "file edit"],
  ["command", "command"],
  ["action", "action"],
];
describe("compact action categories", () => {
  it.each(cases)(
    "renders reconstructed %s operations with their own icon and count",
    (category, label) => {
      const item = tool(category);
      expect(actionCategory({ tool: item })).toBe(category);
      expect(completedActionLabel([{ tool: item }])).toBe(`1 ${label}`);
      const html = renderToStaticMarkup(<CompactToolGroup tools={[item]} />);
      expect(html).toContain(`data-action-category="${category}"`);
      expect(html).toContain(`View actions: 1 ${label}`);
      expect(html).toContain('aria-hidden="true"');
      expect(html).toContain('aria-haspopup="dialog"');
    },
  );
  it("uses canonical operation rather than tool names to distinguish search and read", () => {
    const item = tool("search");
    item.toolName = "read_file";
    expect(actionCategory({ tool: item })).toBe("search");
    if (item.presentation.kind !== "fileRead")
      throw new Error("file-read fixture");
    item.presentation.operation = "glob";
    expect(actionCategory({ tool: item })).toBe("search");
  });
  it("counts operations, not inferred unique files or successful changes", () => {
    const tools = [
      tool("read", "read-a"),
      tool("read", "read-a-again"),
      tool("search"),
      tool("edit"),
      tool("command"),
    ];
    tools[3]!.statusKind = "error";
    const html = renderToStaticMarkup(<CompactToolGroup tools={tools} />);
    expect(html).toContain("2 file reads, 1 search, 1 command, 1 file edit");
    expect(html).toContain("1 action failed");
    expect(html).toContain('data-action-category="edit"');
    expect(html).not.toContain("Edited files");
  });
  it.each(["loading", "denied", "invalid"] as const)(
    "does not classify an unreconstructed %s edit from its label",
    (state) => {
      const item = tool("edit");
      item.reconstruction = { state };
      item.toolName = "edit_file";
      expect(actionCategory({ tool: item })).toBe("action");
    },
  );
  it("preserves running, stopped and unknown status independently of the icon", () => {
    const item = tool("edit");
    item.statusKind = "running";
    expect(renderToStaticMarkup(<CompactToolGroup tools={[item]} />)).toContain(
      'data-action-category="running"',
    );
    item.statusKind = "cancelled";
    const stopped = renderToStaticMarkup(<CompactToolGroup tools={[item]} />);
    expect(stopped).toContain("1 action stopped");
    expect(stopped).toContain('data-action-category="edit"');
    item.statusKind = "unknown";
    expect(renderToStaticMarkup(<CompactToolGroup tools={[item]} />)).toContain(
      "Action status unavailable",
    );
  });
  it("uses deterministic priority regardless of order", () => {
    for (const kinds of [
      ["read", "command", "edit"],
      ["edit", "command", "read"],
    ] as const)
      expect(
        representativeActionCategory(
          actionEntries(kinds.map((kind) => tool(kind))),
        ),
      ).toBe("edit");
    expect(representativeActionCategory([])).toBe("action");
  });
});
