import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeToolItem } from "../src/components/codeTools/CodeToolItem";
import {
  formatDuration,
  stripAnsi,
  toCodeToolView,
} from "../src/components/codeTools/codeTools";
import type { RenderedToolCallView } from "../src/lib/types";

describe("formatDuration", () => {
  it("scales units for operator reading", () => {
    expect(formatDuration(480)).toBe("480ms");
    expect(formatDuration(1230)).toBe("1.2s");
    expect(formatDuration(2000)).toBe("2s");
    expect(formatDuration(134000)).toBe("2m 14s");
    expect(formatDuration(120000)).toBe("2m");
  });
});

describe("stripAnsi", () => {
  it("removes color, cursor, and OSC sequences but keeps text", () => {
    expect(stripAnsi("\x1b[31mred\x1b[0m plain")).toBe("red plain");
    expect(stripAnsi("\x1b]8;;http://x\x07link\x1b]8;;\x07")).toBe("link");
    expect(stripAnsi("no escapes [0m here")).toBe("no escapes [0m here");
  });
});

function commandCall(
  meta: Record<string, unknown>,
  body: string,
): RenderedToolCallView {
  return {
    itemKey: "t1",
    toolName: "bash",
    statusKind: "success",
    args: { rawText: JSON.stringify({ command: "make build" }), fields: [] },
    result: {
      rawText: `gents_exec: ${JSON.stringify(meta)}\n${body}`,
      fields: [],
    },
  } as unknown as RenderedToolCallView;
}

describe("command projection metadata", () => {
  it("carries duration and cwd and strips ANSI from streams", () => {
    const view = toCodeToolView(
      commandCall(
        {
          ok: true,
          status: "success",
          command: "make build",
          exit_code: 0,
          timed_out: false,
          duration_ms: 1230,
          cwd: "/work/repo",
        },
        "stdout:\n\x1b[32mok\x1b[0m done\nstderr:\n(empty)",
      ),
    );
    expect(view).toMatchObject({
      kind: "command",
      durationMs: 1230,
      cwd: "/work/repo",
      stdout: "ok done",
    });
  });
});

describe("CodeToolItem terminal chrome", () => {
  afterEach(() => vi.restoreAllMocks());

  it("renders duration + cwd and copies stdout", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(
      <CodeToolItem
        view={{
          kind: "command",
          command: "make build",
          exitCode: 0,
          executionMode: "read_only",
          networkMode: "disabled",
          durationMs: 134000,
          cwd: "/work/repo",
          timedOut: false,
          failed: false,
          stdout: "all green",
          stderr: "",
        }}
      />,
    );

    expect(screen.getByTestId("code-duration")).toHaveTextContent("2m 14s");
    expect(screen.getByTestId("code-cwd")).toHaveTextContent("/work/repo");

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("all green"));
  });

  it("offers diff copy with +/- prefixes", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(
      <CodeToolItem
        view={{
          kind: "fileEdit",
          path: "src/a.rs",
          created: false,
          replacementsApplied: 1,
          diff: [
            { kind: "del", text: "old line" },
            { kind: "add", text: "new line" },
          ],
        }}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("-old line\n+new line"));
  });
});
