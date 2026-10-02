import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const call = vi.fn();
vi.mock("../src/ui/screens/agent/bridgeCall", () => ({
  call: (...args: unknown[]) => call(...args),
  message: (error: unknown) => String(error),
}));
vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

import { AllowedFoldersPanel } from "../src/ui/screens/agent/AllowedFoldersPanel";

describe("AllowedFoldersPanel", () => {
  beforeEach(() => call.mockReset());

  it("says none are added and that the working folder is readable", async () => {
    call.mockResolvedValue({ dirs: [] });
    render(<AllowedFoldersPanel />);
    await screen.findByText("None added");
    expect(call).toHaveBeenCalledWith("desktop_allowed_dirs_list", {});
    expect(screen.queryByText("Remove")).toBeNull();
  });

  it("toggles the access of a configured folder and removes it", async () => {
    call.mockResolvedValue({ dirs: [{ path: "/docs", access: "read" }] });
    render(<AllowedFoldersPanel />);
    fireEvent.click(await screen.findByText("Read only"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("desktop_allowed_dirs_add", {
        path: "/docs",
        access: "read_write",
      }),
    );
    fireEvent.click(await screen.findByText("Remove"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("desktop_allowed_dirs_remove", {
        path: "/docs",
      }),
    );
  });

  it("adds a typed folder read-only when no picker exists", async () => {
    call.mockResolvedValue({ dirs: [] });
    render(<AllowedFoldersPanel />);
    fireEvent.change(await screen.findByLabelText("Folder path"), {
      target: { value: "/data" },
    });
    fireEvent.click(screen.getByText("Add"));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("desktop_allowed_dirs_add", {
        path: "/data",
        access: "read",
      }),
    );
  });
});
