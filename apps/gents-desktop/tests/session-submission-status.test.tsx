import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SessionSubmissionStatus } from "../src/ui/screens/SessionScreen";

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
});
