import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));
vi.mock("../src/ui/lib/pickDirectory", () => ({
  canPickDirectory: () => true,
  pickDirectory: vi.fn().mockRejectedValue(new Error("dialog unavailable")),
}));

import { ChatFolderPicker } from "../src/ui/screens/ChatFolderPicker";

describe("the chat folder picker", () => {
  it("reports a picker that failed to open", async () => {
    render(<ChatFolderPicker folder={null} onChange={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: /Folder/ }));
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith(
        "Couldn’t choose a folder: dialog unavailable",
      ),
    );
  });
});
