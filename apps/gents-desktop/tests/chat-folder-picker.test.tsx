import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));
vi.mock("../src/lib/nativeShell", () => ({
  canPickDirectory: () => true,
  pickDirectory: vi.fn().mockRejectedValue(new Error("dialog unavailable")),
}));

import { ChatFolderPicker } from "../src/ui/screens/ChatFolderPicker";

const open = async (name: RegExp) => {
  fireEvent.click(screen.getByRole("button", { name }));
  return screen.findByRole("menuitem", { name: "Choose a folder…" });
};

describe("the chat folder picker", () => {
  it("reports a picker that failed to open", async () => {
    render(<ChatFolderPicker folder={null} onChange={vi.fn()} />);
    fireEvent.click(await open(/No folder/));
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith(
        "Couldn’t choose a folder: dialog unavailable",
        { id: "Couldn’t choose a folder: dialog unavailable" },
      ),
    );
  });

  it("shows the current folder from ~, shortened from the left", async () => {
    render(
      <ChatFolderPicker
        folder="/Users/main/Projects/gents/gents-test-workspace"
        home="/Users/main"
        onChange={vi.fn()}
      />,
    );
    await open(/gents-test-workspace/);
    expect(screen.getByText("~/…/gents/gents-test-workspace")).toHaveAttribute(
      "title",
      "/Users/main/Projects/gents/gents-test-workspace",
    );
  });

  it("picks a recent folder again, or none", async () => {
    const onChange = vi.fn();
    render(
      <ChatFolderPicker
        folder="/work/notes"
        recent={["/work/notes", "/work/site"]}
        onChange={onChange}
      />,
    );
    await open(/notes/);
    expect(screen.getByRole("menuitemradio", { name: "notes" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(screen.getByRole("menuitemradio", { name: "site" }));
    expect(onChange).toHaveBeenLastCalledWith("/work/site");
    await open(/notes/);
    fireEvent.click(screen.getByRole("menuitemradio", { name: "No folder" }));
    expect(onChange).toHaveBeenLastCalledWith(null);
  });
});
