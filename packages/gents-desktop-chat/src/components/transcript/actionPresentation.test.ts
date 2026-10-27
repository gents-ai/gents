import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { describe, expect, it } from "vitest";
import {
  actionEntries,
  actionKind,
  actionStatus,
  actionTitle,
  runningAction,
} from "./actionPresentation.js";

function pair(): RenderedToolCallView[] {
  return [
    {
      itemKey: "launcher",
      requestId: "request",
      toolCallId: "spawn",
      toolName: "spawn_process",
      statusKind: "success",
      reconstruction: { state: "ready" },
      presentation: {
        kind: "process",
        action: "spawn",
        target: "bash_unrestricted",
        description: JSON.stringify({ command: "cargo test -p vera-app" }),
        output: JSON.stringify({
          tool_call_id: "spawned:launch",
          status: "running",
        }),
      },
    },
    {
      itemKey: "child",
      requestId: "request",
      toolCallId: "spawned:launch",
      toolName: "bash_unrestricted",
      statusKind: "running",
      awaitMode: "background",
      reconstruction: { state: "loading" },
      presentation: {
        kind: "command",
        command: "bash_unrestricted",
        exitCode: null,
        timedOut: false,
        failed: false,
        durationMs: null,
        cwd: null,
        executionMode: null,
        networkMode: null,
        stdout: "unavailable",
        stderr: "",
        fallbackOutput: null,
      },
    },
  ];
}

describe("action presentation", () => {
  it.each([null, "bash_unrestricted"])(
    "classifies a reconstructed command with display text %s from its native kind",
    (command) => {
      const tool = pair()[1]!;
      tool.reconstruction = { state: "ready" };
      if (tool.presentation.kind !== "command")
        throw new Error("command fixture");
      tool.presentation.command = command;
      expect(actionKind({ tool })).toBe("command");
    },
  );

  it("does not treat an unverified command placeholder as reconstructed output", () => {
    expect(actionKind({ tool: pair()[1]! })).toBeNull();
  });
  it.each([
    ["error", "userCancelled", "cancelled"],
    ["error", "interrupted", "cancelled"],
    ["error", "unknown", "cancelled"],
    ["error", "deadline", "error"],
    ["error", null, "error"],
    ["running", "userCancelled", "running"],
    ["success", "interrupted", "success"],
  ])(
    "presents native status %s with cause %s as %s",
    (statusKind, cause, expected) => {
      const tool = pair()[1]!;
      tool.statusKind = statusKind!;
      tool.cancelCause = cause
        ? {
            cause,
            source: "toolLifecycle",
            confidence: "direct",
            at: null,
            evidence: [],
          }
        : null;
      expect(actionStatus(tool)).toBe(expected);
    },
  );
  it("pairs a launcher with its exact background call and retains both records", () => {
    const tools = pair();
    const original = JSON.stringify(tools);
    const entries = actionEntries(tools);
    expect(entries).toEqual([{ tool: tools[1], launcher: tools[0] }]);
    expect(actionTitle(entries[0]!)).toBe("cargo test -p vera-app");
    expect(runningAction(entries[0]!)).toBe("Running cargo test -p vera-app");
    expect(JSON.stringify(tools)).toBe(original);
  });

  it.each([
    "different request",
    "missing request",
    "different call",
    "foreground",
    "denied launcher",
    "failed launcher",
    "denied command",
  ])("keeps unrelated or unavailable launches separate: %s", (condition) => {
    const tools = pair();
    const child = tools[1]!;
    if (condition === "different request") child.requestId = "another";
    if (condition === "missing request") child.requestId = null;
    if (condition === "different call") child.toolCallId = "another";
    if (condition === "foreground") child.awaitMode = "foreground";
    if (condition === "denied launcher")
      tools[0]!.reconstruction = { state: "denied" };
    if (condition === "failed launcher") tools[0]!.statusKind = "error";
    if (condition === "denied command") {
      child.statusKind = "error";
      child.denial = {
        category: "workspace",
        categoryLabel: "Access denied",
        ruleId: "workspace-root",
        reasonLine: "Outside the allowed root",
        diagnostic: "Choose an allowed path",
      };
    }
    expect(actionEntries(tools)).toHaveLength(2);
    expect(actionTitle({ tool: child })).toBe("Shell command");
  });

  it("does not pick a launcher or child from ambiguous identities", () => {
    const tools = pair();
    expect(
      actionEntries([...tools, { ...tools[0]!, itemKey: "duplicate-launch" }]),
    ).toHaveLength(3);
    expect(
      actionEntries([...tools, { ...tools[1]!, itemKey: "duplicate-child" }]),
    ).toHaveLength(3);
  });

  it.each(["loading", "denied", "invalid"] as const)(
    "does not read a %s payload to make a title",
    (state) => {
      const tool = pair()[1]!;
      tool.reconstruction = { state };
      if (tool.presentation.kind === "command")
        tool.presentation.command = "private command";
      expect(actionTitle({ tool })).toBe("Shell command");
    },
  );
});
