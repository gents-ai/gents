import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SessionSubmissionStatus } from "../src/ui/screens/session/composerParts";

describe("kit session submission status", () => {
  it("announces accepted queued work from the canonical workflow projection", () => {
    render(
      <SessionSubmissionStatus
        activityStatus={{
          kind: "waiting",
          label: "Waiting for the node…",
          detail:
            "The node has not started this request yet. Messages you send now wait behind it.",
          animated: true,
        }}
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent("Waiting for the node…");
    expect(screen.getByRole("status")).toHaveAttribute(
      "title",
      expect.stringContaining("The node has not started this request yet"),
    );
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  describe("the shown session's quiet re-read", () => {
    const refresh = {
      title: "Updating session",
      detail: "Showing the last read of this session while it is read again.",
    };
    afterEach(() => vi.useRealTimers());

    it("shows nothing for a re-read that settles quickly", () => {
      vi.useFakeTimers();
      const { rerender } = render(
        <SessionSubmissionStatus activityStatus={null} refresh={refresh} />,
      );
      act(() => vi.advanceTimersByTime(300));
      rerender(<SessionSubmissionStatus activityStatus={null} refresh={null} />);
      act(() => vi.advanceTimersByTime(1_000));
      expect(screen.queryByText("Updating…")).not.toBeInTheDocument();
    });

    it("says Updating… in the footer once the re-read runs long", () => {
      vi.useFakeTimers();
      render(<SessionSubmissionStatus activityStatus={null} refresh={refresh} />);
      act(() => vi.advanceTimersByTime(400));
      expect(screen.getByRole("status")).toHaveTextContent("Updating…");
      expect(screen.getByRole("status")).toHaveAttribute("title", refresh.detail);
    });

    it("gives way to the session's own activity", () => {
      vi.useFakeTimers();
      render(
        <SessionSubmissionStatus
          activityStatus={{
            kind: "waiting",
            label: "Waiting for the node…",
            detail: "The node has not started this request yet.",
            animated: true,
          }}
          refresh={refresh}
        />,
      );
      act(() => vi.advanceTimersByTime(1_000));
      expect(screen.getByRole("status")).toHaveTextContent("Waiting for the node…");
      expect(screen.queryByText("Updating…")).not.toBeInTheDocument();
    });
  });
});
