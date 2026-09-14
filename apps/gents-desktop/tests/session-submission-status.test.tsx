import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SessionSubmissionStatus } from "../src/ui/screens/SessionScreen";

describe("kit session submission status", () => {
  it("announces accepted queued work from the canonical workflow projection", () => {
    render(
      <SessionSubmissionStatus
        error={null}
        activityStatus={{
          kind: "waiting",
          label: "Waiting for the agent…",
          detail: "Your message is queued until the enrolled agent claims it.",
          animated: true,
        }}
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent("Your message is queued");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
  it("shows submission errors instead of silently swallowing them", () => {
    render(
      <SessionSubmissionStatus
        error="Request rejected: agent unavailable"
        activityStatus={null}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Request rejected: agent unavailable",
    );
  });
});
