import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const call = vi.fn();
vi.mock("../src/ui/screens/agent/bridgeCall", () => ({
  call: (...args: unknown[]) => call(...args),
  message: (error: unknown) => String(error),
}));
vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

import { PluginAccessPrompt } from "../src/ui/screens/PluginAccessPrompt";

const question = {
  id: "q1",
  prompt: "Allow team/ocr to read /data/x.pdf?",
  folder: "/data",
  isDir: false,
};

describe("PluginAccessPrompt", () => {
  beforeEach(() => call.mockReset());

  it("shows nothing while no plugin is waiting", async () => {
    call.mockResolvedValue({ requests: [] });
    render(<PluginAccessPrompt />);
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("desktop_plugin_approvals_pending"),
    );
    expect(screen.queryByText("Allow once")).toBeNull();
  });

  it.each([
    ["Allow once", "once"],
    ["Always allow this file", "file"],
    ["Always allow this folder", "always"],
    ["Deny", "deny"],
  ])("sends %s as %s", async (label, decision) => {
    call.mockImplementation(async (command: string) =>
      command === "desktop_plugin_approvals_pending" ? { requests: [question] } : null,
    );
    render(<PluginAccessPrompt />);
    await screen.findByText(question.prompt);
    fireEvent.click(screen.getByText(label));
    await waitFor(() =>
      expect(call).toHaveBeenCalledWith("desktop_plugin_approval_decide", {
        id: "q1",
        decision,
      }),
    );
  });
});
