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
    name: /View actions: Ran 2 commands.*1 command failed/,
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
  [1, 1, "1 command failed · 1 action failed"],
  [2, 2, "2 commands failed · 2 actions failed"],
] as const)(
  "identifies %i command and %i action failures consistently",
  (commands, actions, label) => {
    const failures = [
      ...Array.from({ length: commands }, (_, i) =>
        tool(`command-${i}`, "error"),
      ),
      ...Array.from({ length: actions }, (_, i) => ({
        ...tool(`action-${i}`, "error"),
        toolName: "read_file",
        presentation: {
          kind: "generic" as const,
          summary: "Read file",
          body: null,
        },
      })),
    ];
    render(
      <CompactToolGroup tools={[tool("success", "success"), ...failures]} />,
    );
    const row = screen.getByRole("button", {
      name: `View actions: Ran 1 command · ${label}`,
    });
    expect(row.querySelector(".tool-activity-errors")?.textContent).toBe(label);
    expect(row.querySelector("[title]")?.getAttribute("title")).toBe(
      `Ran 1 command · ${label}`,
    );
  },
);

it("does not count successful stderr warnings or native cancellation as failure", () => {
  const warning = tool("warning", "success");
  if (warning.presentation.kind === "command")
    warning.presentation.stderr = "warning: not a failure";
  const cancelled = {
    ...tool("cancelled", "error"),
    cancelCause: {
      cause: "userCancelled",
      source: "toolLifecycle",
      confidence: "certain",
      at: null,
      evidence: [],
    },
  };
  render(<CompactToolGroup tools={[warning, cancelled]} />);
  const row = screen.getByRole("button", {
    name: "View actions: Ran 1 command · 1 action stopped",
  });
  expect(row.querySelector(".tool-activity-errors")).toBeNull();
});

it("keeps failure-only captions explicit when command text is unavailable", () => {
  const command = tool("command", "error");
  if (command.presentation.kind === "command")
    command.presentation.command = null;
  render(<CompactToolGroup tools={[command]} />);
  const row = screen.getByRole("button", {
    name: "View actions: 1 command failed",
  });
  expect(row.querySelector(".tool-activity-label")?.textContent).toBe(
    "1 command failed",
  );
  expect(row.querySelector("[title]")?.getAttribute("title")).toBe(
    "1 command failed",
  );
  expect(row.querySelector(".tool-activity-errors")).toBeNull();
});
