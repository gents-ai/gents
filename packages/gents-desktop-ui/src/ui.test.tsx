import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ConfirmDialog } from "./ConfirmDialog.js";
import { CopyButton } from "./CopyButton.js";
import { formatMessageTime } from "./formatTime.js";

describe("shared desktop UI", () => {
  it("keeps a closed confirmation dialog out of the tree", () => {
    render(
      <ConfirmDialog
        open={false}
        title="Remove peer?"
        message="This cannot be undone."
        onCancel={vi.fn()}
        onConfirm={vi.fn()}
      />,
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("supports confirmation and escape cancellation", () => {
    const onCancel = vi.fn();
    const onConfirm = vi.fn();
    render(
      <ConfirmDialog
        open
        title="Remove peer?"
        message="This cannot be undone."
        onCancel={onCancel}
        onConfirm={onConfirm}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(onConfirm).toHaveBeenCalledOnce();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onCancel).toHaveBeenCalledOnce();
  });

  it("copies lazily supplied text", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    render(<CopyButton getText={() => "latest output"} />);
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    expect(writeText).toHaveBeenCalledWith("latest output");
  });

  it("omits invalid timestamps", () => {
    expect(formatMessageTime("not-a-date")).toBeNull();
  });
});
