import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
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
  beforeEach(() => {
    call.mockReset();
  });

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

  it("does not bring back a request answered while a poll was out", async () => {
    let late!: (value: { requests: (typeof question)[] }) => void;
    let polls = 0;
    call.mockImplementation(async (command: string) => {
      if (command === "desktop_plugin_approval_decide") return null;
      polls += 1;
      if (polls === 1) return { requests: [question] };
      /* the host read its queue before the answer reached it */
      return new Promise((resolve) => (late = resolve));
    });
    /* the clock is held only to send the second poll on its tick */
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      render(<PluginAccessPrompt />);
      await screen.findByText(question.prompt);
      act(() => vi.advanceTimersByTime(1000));
    } finally {
      vi.useRealTimers();
    }
    expect(polls).toBe(2);

    fireEvent.click(screen.getByText("Allow once"));
    await waitFor(() => expect(screen.queryByText(question.prompt)).toBeNull());

    await act(async () => late({ requests: [question] }));
    expect(screen.queryByText(question.prompt)).toBeNull();
  });
});
