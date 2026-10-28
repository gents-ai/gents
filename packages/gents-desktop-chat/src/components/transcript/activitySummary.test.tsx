import { renderToStaticMarkup } from "react-dom/server";
import type {
  RenderedToolCallView,
  ToolPresentationView,
} from "@source-inc/gents-desktop-client";
import { expect, it } from "vitest";

import { CompactToolGroup } from "./CompactToolGroup.js";

function tool(
  presentation: ToolPresentationView,
  itemKey = "tool",
): RenderedToolCallView {
  return {
    itemKey,
    toolName: "opaque",
    statusKind: "success",
    reconstruction: { state: "ready" },
    presentation,
  };
}

function read(operation: string): ToolPresentationView {
  return {
    kind: "fileRead",
    operation,
    target: "src/main.rs",
    returnedCount: 1,
    totalCount: 1,
    truncated: false,
    body: "contents",
    fallbackOutput: null,
  };
}

function edit(applied?: boolean | null): ToolPresentationView {
  return {
    kind: "fileEdit",
    operation: "edit_file",
    path: "src/main.rs",
    applied,
    created: false,
    replacementsApplied: 1,
    diff: [],
    fallbackOutput: null,
  };
}

const command: ToolPresentationView = {
  kind: "command",
  command: "opaque",
  exitCode: 0,
  timedOut: false,
  failed: false,
  durationMs: 1,
  cwd: null,
  executionMode: null,
  networkMode: null,
  stdout: "",
  stderr: "",
  fallbackOutput: null,
};

it.each([
  [read("read_file"), "read", "Read files"],
  [read("grep"), "search", "Searched files"],
  [read("glob"), "search", "Searched files"],
  [read("list_files"), "explore", "Explored files"],
  [edit(true), "edit", "Edited files"],
  [edit(false), "fileAction", "Ran 1 file action"],
  [edit(null), "fileAction", "Ran 1 file action"],
  [edit(), "fileAction", "Ran 1 file action"],
  [command, "command", "Ran 1 command"],
  [
    {
      kind: "generic",
      summary: "read_file edited files",
      input: null,
      output: null,
    } satisfies ToolPresentationView,
    "action",
    "Ran 1 action",
  ],
] as const)(
  "uses the native presentation for %s",
  (presentation, kind, label) => {
    const html = renderToStaticMarkup(
      <CompactToolGroup tools={[tool(presentation)]} />,
    );
    expect(html).toContain(`data-activity-kind="${kind}"`);
    expect(html).toContain(`View actions: ${label}`);
  },
);

it("prioritizes confirmed edits and orders mixed summaries consistently without changing the actions", () => {
  const tools = [
    tool(command, "command"),
    tool(read("read_file"), "read"),
    tool(edit(true), "edit"),
  ];
  const before = JSON.stringify(tools);
  const html = renderToStaticMarkup(<CompactToolGroup tools={tools} />);
  expect(html).toContain('data-activity-kind="edit"');
  expect(html).toContain("Edited files, read files, ran 1 command");
  expect(JSON.stringify(tools)).toBe(before);
});

it("does not claim successful file reads or edits from a failed action", () => {
  const tools = [
    tool(read("read_file"), "read"),
    tool(edit(false), "edit"),
  ].map((entry) => ({ ...entry, statusKind: "error" }));
  const html = renderToStaticMarkup(<CompactToolGroup tools={tools} />);
  expect(html).toContain("Ran 2 file actions");
  expect(html).toContain("2 actions failed");
  expect(html).not.toContain("Edited files");
  expect(html).not.toContain("Read files");
});

it("keeps unavailable evidence generic even if its name and hidden payload describe an edit", () => {
  const entry = {
    ...tool(edit(true)),
    toolName: "edit_file",
    reconstruction: { state: "denied" as const },
  };
  const html = renderToStaticMarkup(<CompactToolGroup tools={[entry]} />);
  expect(html).toContain('data-activity-kind="action"');
  expect(html).toContain("Ran 1 action");
  expect(html).not.toContain("Edited files");
});

it("retains the running spinner and cancellation summary", () => {
  const running = renderToStaticMarkup(
    <CompactToolGroup tools={[{ ...tool(command), statusKind: "running" }]} />,
  );
  expect(running).toContain('data-activity-kind="running"');
  expect(running).toContain("<circle");
  const stopped = renderToStaticMarkup(
    <CompactToolGroup
      tools={[{ ...tool(command), statusKind: "cancelled" }]}
    />,
  );
  expect(stopped).toContain("1 action stopped");
  expect(stopped).not.toContain("Ran 1 command");
});
