import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CodeToolItem } from "../src/components/codeTools/CodeToolItem";
import { toCodeToolView } from "../src/components/codeTools/codeTools";
import type { RenderedToolCallView } from "../src/lib/types";

function toolCall(overrides: Partial<RenderedToolCallView>): RenderedToolCallView {
  return {
    itemKey: "tool-1",
    toolName: "bash",
    statusKind: "success",
    args: null,
    result: null,
    ...overrides,
  };
}

function detail(rawText: string) {
  return { rawText, fields: [] };
}

// Result fixtures mirror the runtime's envelopes exactly: `defra_fs: {json}` +
// a human body line (toolset/file_tools.rs) and `defra_exec: {json}` +
// `stdout:\n…\nstderr:\n…` framing (toolset/shared/command.rs).
describe("toCodeToolView", () => {
  it("projects edit_file into a replacement diff", () => {
    const view = toCodeToolView(
      toolCall({
        toolName: "edit_file",
        args: detail(
          JSON.stringify({
            path: "src/main.rs",
            old_text: "let x = 1;",
            new_text: "let x = 2;",
          }),
        ),
        result: detail(
          'defra_fs: {"ok":true,"status":"success","tool":"edit_file","path":"src/main.rs","returned_count":1,"total_count":1,"truncated":false,"replacements_applied":1,"replace_all":false,"bytes_written":10}\nedit_file: edited src/main.rs (1 replacement)',
        ),
      }),
    );
    expect(view).toEqual({
      kind: "fileEdit",
      path: "src/main.rs",
      created: false,
      replacementsApplied: 1,
      diff: [
        { kind: "del", text: "let x = 1;" },
        { kind: "add", text: "let x = 2;" },
      ],
    });
  });

  it("surfaces replace_all multi-site replacements", () => {
    const view = toCodeToolView(
      toolCall({
        toolName: "edit_file",
        args: detail(
          JSON.stringify({
            path: "src/lib.rs",
            old_text: "foo",
            new_text: "bar",
            replace_all: true,
          }),
        ),
        result: detail(
          'defra_fs: {"ok":true,"status":"success","tool":"edit_file","path":"src/lib.rs","returned_count":12,"total_count":12,"truncated":false,"replacements_applied":12,"replace_all":true,"bytes_written":240}\nedit_file: edited src/lib.rs (12 replacements)',
        ),
      }),
    );
    expect(view).toMatchObject({ kind: "fileEdit", replacementsApplied: 12 });
  });

  it("labels write_file created only when the envelope says so", () => {
    const base = {
      toolName: "write_file",
      args: detail(JSON.stringify({ path: "README.md", content: "line 1\nline 2\n" })),
    };
    const created = toCodeToolView(
      toolCall({
        ...base,
        result: detail(
          'defra_fs: {"ok":true,"status":"success","tool":"write_file","path":"README.md","returned_count":0,"total_count":0,"truncated":false,"bytes_written":14,"created":true}\nwrite_file: wrote 14 bytes to README.md',
        ),
      }),
    );
    expect(created).toMatchObject({
      kind: "fileEdit",
      created: true,
      // Trailing newline in content must not produce a spurious blank line.
      diff: [
        { kind: "add", text: "line 1" },
        { kind: "add", text: "line 2" },
      ],
    });

    const overwrote = toCodeToolView(
      toolCall({
        ...base,
        result: detail(
          'defra_fs: {"ok":true,"status":"success","tool":"write_file","path":"README.md","returned_count":0,"total_count":0,"truncated":false,"bytes_written":14,"created":false}\nwrite_file: wrote 14 bytes to README.md',
        ),
      }),
    );
    expect(overwrote).toMatchObject({ kind: "fileEdit", created: false });
  });

  it("parses the stdout/stderr framing and prefers the envelope's full command line", () => {
    const view = toCodeToolView(
      toolCall({
        toolName: "bash",
        // Exec-style: the raw arg carries only the executable.
        args: detail(JSON.stringify({ command: "cargo", args: ["test", "--release"] })),
        result: detail(
          'defra_exec: {"ok":true,"status":"success","command":"cargo test --release","exit_code":0,"timed_out":false,"execution_mode":"read_only","network_mode":"disabled"}\nstdout:\ntest result: ok. 3 passed\nstderr:\n(empty)',
        ),
      }),
    );
    expect(view).toEqual({
      kind: "command",
      command: "cargo test --release",
      exitCode: 0,
      executionMode: "read_only",
      networkMode: "disabled",
      timedOut: false,
      failed: false,
      stdout: "test result: ok. 3 passed",
      stderr: "",
    });
  });

  it("marks a non-zero exit as failed and keeps stderr separate", () => {
    const view = toCodeToolView(
      toolCall({
        toolName: "bash",
        args: detail(JSON.stringify({ command: "cargo build" })),
        result: detail(
          'defra_exec: {"ok":false,"status":"exit_nonzero","command":"cargo build","exit_code":101,"timed_out":false}\nstdout:\n(empty)\nstderr:\nerror[E0425]',
        ),
      }),
    );
    expect(view).toMatchObject({
      kind: "command",
      exitCode: 101,
      failed: true,
      stdout: "",
      stderr: "error[E0425]",
    });
  });

  it("marks a timed-out command as failed even with a null exit code", () => {
    // Timeouts complete the tool call (statusKind success) with exit_code null;
    // the envelope is the only failure signal.
    const view = toCodeToolView(
      toolCall({
        toolName: "bash",
        statusKind: "success",
        args: detail(JSON.stringify({ command: "sleep 999" })),
        result: detail(
          'defra_exec: {"ok":false,"status":"timeout","command":"sleep 999","exit_code":null,"timed_out":true}\nstdout:\n(empty)\nstderr:\n(empty)',
        ),
      }),
    );
    expect(view).toMatchObject({
      kind: "command",
      exitCode: null,
      timedOut: true,
      failed: true,
    });
  });

  it("returns null for a non-code tool", () => {
    expect(
      toCodeToolView(toolCall({ toolName: "web_search", args: detail("{}") })),
    ).toBeNull();
  });
});

describe("CodeToolItem", () => {
  it("renders a file edit as a diff with the path and replacement count", () => {
    render(
      <CodeToolItem
        view={{
          kind: "fileEdit",
          path: "src/main.rs",
          created: false,
          replacementsApplied: 12,
          diff: [
            { kind: "del", text: "old line" },
            { kind: "add", text: "new line" },
          ],
        }}
      />,
    );
    expect(screen.getByTestId("code-file-edit")).toHaveTextContent("src/main.rs");
    expect(screen.getByTestId("code-replacements")).toHaveTextContent("×12");
    const diff = screen.getByTestId("code-diff");
    expect(diff).toHaveTextContent("old line");
    expect(diff).toHaveTextContent("new line");
  });

  it("renders a command as a terminal block with exit code, output, and stderr", () => {
    render(
      <CodeToolItem
        view={{
          kind: "command",
          command: "cargo test",
          exitCode: 0,
          executionMode: "read_only",
          networkMode: "disabled",
          timedOut: false,
          failed: false,
          stdout: "test result: ok",
          stderr: "warning: unused import",
        }}
      />,
    );
    expect(screen.getByTestId("code-command")).toHaveTextContent("cargo test");
    expect(screen.getByTestId("code-exit")).toHaveTextContent("exit 0");
    expect(screen.getByTestId("code-terminal")).toHaveTextContent("test result: ok");
    expect(screen.getByTestId("code-stderr")).toHaveTextContent(
      "warning: unused import",
    );
  });

  it("badges a timed-out command", () => {
    render(
      <CodeToolItem
        view={{
          kind: "command",
          command: "sleep 999",
          exitCode: null,
          executionMode: null,
          networkMode: null,
          timedOut: true,
          failed: true,
          stdout: "",
          stderr: "",
        }}
      />,
    );
    expect(screen.getByTestId("code-exit")).toHaveTextContent("timed out");
  });
});
