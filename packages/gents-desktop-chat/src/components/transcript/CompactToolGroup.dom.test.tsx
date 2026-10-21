// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { CompactToolGroup } from "./CompactToolGroup.js";

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
    name: /View actions: Ran 2 commands.*1 action failed/,
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
