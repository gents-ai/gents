import { describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createElement } from "react";
import type { TaskHook } from "@source-inc/gents-desktop-client/generated/TaskHook";
import {
  formatCommand,
  HooksRows,
  hooksFromDraft,
  splitCommand,
  toHookDraft,
} from "../src/ui/screens/agent/HooksRows";

vi.mock("../src/lib/nativeShell", () => ({}));

const hook = (command: string[]): TaskHook => ({
  hook_id: "verify",
  phase: "after_success",
  command,
  timeout_secs: 120,
});

const CASES: string[][] = [
  ["cargo", "test"],
  ["git", "commit", "-m", ""],
  ["echo", '"hi"'],
  ["C:\\Program Files\\Git\\bin\\git.exe", "status"],
  ["C:\\tools\\run.exe", "--dir", "D:\\My Documents\\out"],
  ["sh", "-c", 'echo "a b" && printf "%s\\n" \\"x\\"'],
  ["printf", "tab\there", "new\nline"],
  ["echo", "it's", "\\", "\\\\", '"', ""],
  ["", ""],
];

describe("task hook commands", () => {
  it("lets a user enter skip codes in a before hook", () => {
    const change = vi.fn();
    const draft = toHookDraft({ ...hook(["./gate"]), phase: "before" });
    try {
      render(
        createElement(HooksRows, { id: "task", value: [draft], onChange: change }),
      );
      fireEvent.change(screen.getByRole("textbox", { name: "Skip exit codes" }), {
        target: { value: "10, 11" },
      });
      expect(hooksFromDraft(change.mock.lastCall![0])).toEqual([
        { ...hook(["./gate"]), phase: "before", skip_exit_codes: [10, 11] },
      ]);
    } finally {
      cleanup();
    }
  });
  it("offers skip codes only on before hooks, while still letting stale codes be cleared", () => {
    try {
      render(
        createElement(HooksRows, {
          id: "task",
          value: [
            toHookDraft(hook(["./after"])),
            { ...toHookDraft(hook(["./stale"])), skipExitCodes: "10" },
          ],
          onChange: vi.fn(),
        }),
      );
      const [after, stale] = screen.getAllByRole("textbox", {
        name: "Skip exit codes",
      });
      expect(after).toBeDisabled();
      expect(stale).toBeEnabled();
    } finally {
      cleanup();
    }
  });
  it("preserves configured gate exit codes when editing other hook fields", () => {
    const gate: TaskHook = {
      ...hook(["./gate"]),
      phase: "before",
      skip_exit_codes: [10, 11],
    };
    const draft = toHookDraft(gate);
    expect(hooksFromDraft([draft])).toEqual([gate]);
    expect(hooksFromDraft([{ ...draft, command: "./gate --verbose" }])).toEqual([
      { ...gate, command: ["./gate", "--verbose"] },
    ]);
  });

  it("rejects invalid skip codes and codes on non-before hooks", () => {
    const gate = toHookDraft({ ...hook(["./gate"]), phase: "before" });
    for (const skipExitCodes of ["0", "256", "1,", "1.5", "-1"]) {
      expect(hooksFromDraft([{ ...gate, skipExitCodes }])).toMatch(/skip codes/);
    }
    expect(
      hooksFromDraft([{ ...gate, phase: "finally", skipExitCodes: "10" }]),
    ).toMatch(/only supported before/);
  });

  it("round-trips every argv through its one-line form", () => {
    for (const argv of CASES) {
      expect(splitCommand(formatCommand(argv)), JSON.stringify(argv)).toEqual(argv);
    }
  });

  it("writes an untouched hook back exactly as stored", () => {
    for (const argv of CASES) {
      const saved = hooksFromDraft([toHookDraft(hook(argv))]);
      expect(saved).toEqual([hook(argv)]);
    }
  });

  it("parses an edited line, keeping typed Windows backslashes literal", () => {
    const draft = toHookDraft(hook(["cargo", "test"]));
    const edited = {
      ...draft,
      command: 'C:\\tools\\x.exe "C:\\Program Files\\y" -m ""',
    };
    expect(hooksFromDraft([edited])).toEqual([
      hook(["C:\\tools\\x.exe", "C:\\Program Files\\y", "-m", ""]),
    ]);
  });

  it("re-parses only the edited row", () => {
    const untouched = toHookDraft(hook(["echo", '"hi"']));
    const edited = { ...toHookDraft({ ...hook(["a"]), hook_id: "b" }), command: "b c" };
    expect(hooksFromDraft([untouched, edited])).toEqual([
      hook(["echo", '"hi"']),
      { ...hook(["b", "c"]), hook_id: "b" },
    ]);
  });
});
