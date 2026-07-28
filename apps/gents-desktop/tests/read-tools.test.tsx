import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeToolItem } from "@source-inc/gents-desktop-chat";
import { toCodeToolView } from "@source-inc/gents-desktop-chat";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

function readCall(
  toolName: string,
  argsJson: Record<string, unknown>,
  meta: Record<string, unknown>,
  body: string,
): RenderedToolCallView {
  return {
    itemKey: "r1",
    toolName,
    statusKind: "success",
    args: { rawText: JSON.stringify(argsJson), fields: [] },
    result: { rawText: `gents_fs: ${JSON.stringify(meta)}\n${body}`, fields: [] },
  } as unknown as RenderedToolCallView;
}

describe("file-read projection", () => {
  it("projects read_file with counts and body, dropping the JSON head", () => {
    const view = toCodeToolView(
      readCall(
        "read_file",
        { path: "src/main.rs" },
        {
          ok: true,
          status: "success",
          tool: "read_file",
          path: "src/main.rs",
          returned_count: 120,
          total_count: 400,
          truncated: true,
        },
        "1: fn main() {}\n2: // done",
      ),
    );
    expect(view).toMatchObject({
      kind: "fileRead",
      tool: "read_file",
      target: "src/main.rs",
      returnedCount: 120,
      totalCount: 400,
      truncated: true,
      body: "1: fn main() {}\n2: // done",
    });
  });

  it("falls back to the args pattern for glob and stays honest without metadata", () => {
    const globView = toCodeToolView(
      readCall(
        "glob",
        { pattern: "**/*.rs" },
        {
          ok: true,
          status: "success",
          tool: "glob",
          returned_count: 8,
          total_count: 8,
          truncated: false,
        },
        "src/a.rs\nsrc/b.rs",
      ),
    );
    expect(globView).toMatchObject({ kind: "fileRead", target: "**/*.rs" });

    const noMeta = toCodeToolView({
      itemKey: "r2",
      toolName: "grep",
      statusKind: "success",
      args: { rawText: JSON.stringify({ pattern: "todo" }), fields: [] },
      result: { rawText: "not an envelope", fields: [] },
    } as unknown as RenderedToolCallView);
    expect(noMeta).toBeNull();
  });
});

describe("file-read rendering", () => {
  afterEach(() => vi.restoreAllMocks());

  it("renders collapsed with a count summary and copyable body", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(
      <CodeToolItem
        view={{
          kind: "fileRead",
          tool: "grep",
          target: "todo",
          returnedCount: 12,
          totalCount: 12,
          truncated: false,
          body: "src/a.rs:L10: // TODO fix",
        }}
      />,
    );

    const details = screen.getByTestId("code-file-read");
    expect(details).not.toHaveAttribute("open");
    expect(screen.getByTestId("code-read-counts")).toHaveTextContent("12 matches");

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("src/a.rs:L10: // TODO fix"),
    );
  });
});
