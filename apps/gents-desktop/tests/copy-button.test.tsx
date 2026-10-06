import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CopyButton } from "../src/ui/screens/Markdown";

afterEach(() => vi.unstubAllGlobals());

describe("a copy button", () => {
  it("claims a copy only once the clipboard has taken it", async () => {
    vi.stubGlobal("navigator", {
      clipboard: { writeText: vi.fn().mockRejectedValue(new Error("denied")) },
    });
    render(<CopyButton getText={() => "text"} />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    });
    expect(
      screen.getByRole("button", { name: "Copy" }).querySelector(".lucide-check"),
    ).toBeNull();
  });

  it("shows the copy once the clipboard has it", async () => {
    vi.stubGlobal("navigator", {
      clipboard: { writeText: vi.fn().mockResolvedValue(undefined) },
    });
    render(<CopyButton getText={() => "text"} />);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    });
    expect(
      screen.getByRole("button", { name: "Copy" }).querySelector(".lucide-check"),
    ).not.toBeNull();
  });
});
