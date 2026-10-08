import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

import { PluginAccessPrompt } from "../src/ui/screens/PluginAccessPrompt";
import { renderIn, testApp } from "./app-fixture";

const pending = vi.fn();
const decide = vi.fn();
const show = () =>
  renderIn(
    testApp({
      api: { listPendingPluginApprovals: pending, decidePluginApproval: decide },
    }),
    <PluginAccessPrompt />,
  );

const question = {
  id: "q1",
  prompt: "Allow team/ocr to read /data/x.pdf?",
  folder: "/data",
  isDir: false,
};

describe("PluginAccessPrompt", () => {
  beforeEach(() => {
    pending.mockReset();
    decide.mockReset();
    decide.mockResolvedValue(undefined);
  });

  it("shows nothing while no plugin is waiting", async () => {
    pending.mockResolvedValue({ requests: [] });
    show();
    await waitFor(() => expect(pending).toHaveBeenCalled());
    expect(screen.queryByText("Allow once")).toBeNull();
  });

  it.each([
    ["Allow once", "once"],
    ["Always allow this file", "file"],
    ["Always allow this folder", "always"],
    ["Deny", "deny"],
  ])("sends %s as %s", async (label, decision) => {
    pending.mockResolvedValue({ requests: [question] });
    show();
    await screen.findByText(question.prompt);
    fireEvent.click(screen.getByText(label));
    await waitFor(() => expect(decide).toHaveBeenCalledWith("q1", decision));
  });

  it("does not bring back a request answered while a poll was out", async () => {
    let late!: (value: { requests: (typeof question)[] }) => void;
    let polls = 0;
    pending.mockImplementation(async () => {
      polls += 1;
      if (polls === 1) return { requests: [question] };
      /* the host read its queue before the answer reached it */
      return new Promise((resolve) => (late = resolve));
    });
    /* the clock is held only to send the second poll on its tick */
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      show();
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
