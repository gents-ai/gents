import { describe, expect, it } from "vitest";
import type { TaskHook } from "@source-inc/gents-desktop-client";
import {
  formatCommand,
  hooksFromDraft,
  splitCommand,
  toHookDraft,
} from "../src/ui/screens/agent/HooksRows";

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

  it("keeps the regressions from the review", () => {
    const empty = hooksFromDraft([toHookDraft(hook(["git", "commit", "-m", ""]))]);
    expect(empty).toEqual([hook(["git", "commit", "-m", ""])]);
    const quoted = hooksFromDraft([toHookDraft(hook(["echo", '"hi"']))]);
    expect(quoted).toEqual([hook(["echo", '"hi"'])]);
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
