// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { CompactToolGroup } from "./CompactToolGroup.js";
import { MessageList } from "../Transcript.js";

HTMLDialogElement.prototype.showModal = function () {
  this.open = true;
};
HTMLDialogElement.prototype.close = function () {
  this.open = false;
  this.dispatchEvent(new Event("close"));
};
HTMLElement.prototype.scrollIntoView = vi.fn();
afterEach(cleanup);

function tool(itemKey: string, statusKind: string): RenderedToolCallView {
  return {
    itemKey,
    toolName: "bash",
    statusKind,
    reconstruction: { state: "ready" },
    presentation: {
      kind: "command",
      command: `command-${itemKey}`,
      exitCode: statusKind === "error" ? 1 : 0,
      timedOut: false,
      failed: statusKind === "error",
      durationMs: 1,
      cwd: null,
      executionMode: null,
      networkMode: null,
      stdout: `output-${itemKey}`,
      stderr: null,
      fallbackOutput: null,
    },
  };
}

it("opens the actual failed action from mixed history while retaining every chronological detail", () => {
  render(
    <CompactToolGroup
      tools={[
        tool("first", "success"),
        tool("failed", "error"),
        tool("last", "success"),
        tool("active", "running"),
      ]}
    />,
  );
  const summary = screen.getByRole("button", {
    name: /View actions: 3 commands.*1 command failed/,
  });
  fireEvent.click(summary);
  const dialog = screen.getByRole("dialog", { name: /Actions 4/ });
  const details = Array.from(
    dialog.querySelectorAll<HTMLDetailsElement>(".tool-item"),
  );
  expect(
    details.map((item) => item.querySelector(".tool-primary")?.textContent),
  ).toEqual([
    "command-first",
    "command-failed",
    "command-last",
    "command-active",
  ]);
  expect(details.map((item) => item.open)).toEqual([false, true, false, false]);
  expect(dialog.textContent).toContain("output-failed");
  fireEvent.click(screen.getByRole("button", { name: "Close actions" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(summary);
});

it("keeps a long interrupted block to three rows and opens every original action in order", () => {
  const requestTurn = {
    requestDocId: "physical",
    requestId: "request",
    settled: false,
  };
  const timelineItems: RenderedTimelineItem[] = Array.from(
    { length: 24 },
    (_, index) => [
      {
        kind: "toolGroup" as const,
        itemKey: `group-${index}`,
        tools: [
          {
            ...tool(
              String(index),
              index >= 21 ? "running" : index === 3 ? "error" : "success",
            ),
            requestTurn,
          },
        ],
      },
      {
        kind: "assistantMessage" as const,
        itemKey: `blank-${index}`,
        content: "",
        requestTurn,
        reconstruction: { state: "ready" as const },
      },
    ],
  ).flat();
  render(<MessageList timelineItems={timelineItems} compactTools />);
  const rows = screen.getAllByRole("button", { name: /^View actions:/ });
  expect(rows).toHaveLength(3);
  expect(rows[1]?.textContent).toContain("2 actions running");
  expect(rows[2]?.textContent).toContain("command-23");
  fireEvent.click(rows[0]!);
  const dialog = screen.getByRole("dialog", { name: "Actions 24" });
  const details = Array.from(
    dialog.querySelectorAll<HTMLDetailsElement>(".tool-item"),
  );
  expect(
    details.map((item) => item.querySelector(".tool-primary")?.textContent),
  ).toEqual(Array.from({ length: 24 }, (_, index) => `command-${index}`));
  expect(details[3]?.open).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "Close actions" }));
  expect(document.activeElement).toBe(rows[0]);
});

it.each([
  [1, 0, "1 command failed"],
  [2, 0, "2 commands failed"],
  [0, 1, "1 action failed"],
  [0, 2, "2 actions failed"],
  [2, 1, "2 commands, 1 action failed"],
])(
  "identifies %s failed commands and %s failed other actions",
  (commands, actions, label) => {
    const failedCommands = Array.from({ length: commands }, (_, index) =>
      tool(`command-${index}`, "error"),
    );
    const failedActions = Array.from(
      { length: actions },
      (_, index): RenderedToolCallView => ({
        ...tool(`action-${index}`, "error"),
        toolName: "read_file",
        presentation: {
          kind: "fileRead",
          operation: "read_file",
          target: "file.rs",
          returnedCount: 0,
          totalCount: 0,
          truncated: false,
          body: "",
          fallbackOutput: null,
        },
      }),
    );
    render(
      <CompactToolGroup
        tools={[
          tool("success", "success"),
          ...failedCommands,
          ...failedActions,
        ]}
      />,
    );
    const total = `${1 + commands} ${commands ? "commands" : "command"}${actions ? `, ${actions} ${actions === 1 ? "file read" : "file reads"}` : ""}`;
    const summary = screen.getByRole("button", {
      name: `View actions: ${total} · ${label}`,
    });
    expect(
      summary.querySelector(".tool-activity-errors")?.getAttribute("title"),
    ).toBe(label);
    expect(summary.querySelector(".tool-activity-errors")?.textContent).toBe(
      label,
    );
    expect(
      summary.querySelector(".tool-activity-label")?.getAttribute("title"),
    ).toBe(`${total} · ${label}`);
  },
);

it("keeps successful stderr warnings and cancelled commands out of failure counts", () => {
  const success = tool("warning", "success");
  if (success.presentation.kind === "command")
    success.presentation.stderr = "Warning: color settings conflict";
  const cancelled = {
    ...tool("cancelled", "error"),
    cancelCause: {
      cause: "userCancelled",
      source: "requestInterrupt",
      confidence: "direct",
      at: null,
      evidence: [],
    },
  } satisfies RenderedToolCallView;
  render(<CompactToolGroup tools={[success, cancelled]} />);
  const summary = screen.getByRole("button", {
    name: "View actions: 1 command · 1 action stopped",
  });
  expect(summary.querySelector(".tool-activity-errors")).toBeNull();
});

it.each([
  `python3 -c '${"x".repeat(32 * 1024)}'`,
  `${"x".repeat(246)}🔧${"y".repeat(1024)}`,
])(
  "bounds automatic activity text while retaining the complete command in details %#",
  (command) => {
    const entry = tool("large", "running");
    if (entry.presentation.kind !== "command")
      throw new Error("command fixture");
    entry.presentation.command = command;
    render(<CompactToolGroup tools={[entry]} />);
    const row = screen.getByRole("button", { name: /^View actions: Running / });
    expect(row.textContent!.length).toBeLessThanOrEqual(256);
    expect(row.getAttribute("aria-label")!.length).toBeLessThanOrEqual(270);
    expect(
      row.querySelector(".tool-activity-label")!.getAttribute("title")!.length,
    ).toBeLessThanOrEqual(256);
    expect(row.textContent).toMatch(/…$/u);
    expect(row.textContent).not.toMatch(/[\uD800-\uDBFF]…$/u);
    fireEvent.click(row);
    const dialog = screen.getByRole("dialog", { name: /Actions 1/ });
    expect(dialog.textContent).toContain(command);
    expect(entry.presentation.command).toBe(command);
  },
);
