import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { CancelCauseBadge, CancelCauseDetails } from "../src/components/cancelUx";
import type { DerivedCancelCauseView } from "../src/lib/types/operations";

const userCancelled: DerivedCancelCauseView = {
  cause: "userCancelled",
  source: "requestInterrupt",
  confidence: "direct",
  at: "2026-05-20T10:32:14Z",
  evidence: [
    "AgentRequest.interrupt_requested_at = 2026-05-20T10:32:14Z",
    "no parent cascade (caused_by_parent_request_id is null)",
  ],
};

const deadline: DerivedCancelCauseView = {
  cause: "deadline",
  source: "toolLifecycle",
  confidence: "derived",
  at: "2026-05-20T10:35:02Z",
  evidence: ['AgentToolCall.lifecycle_state = "timedOut"'],
};

const unknown: DerivedCancelCauseView = {
  cause: "unknown",
  source: "unresolved",
  confidence: "derived",
  at: null,
  evidence: ["checked: no parent cascade"],
};

describe("CancelCauseBadge", () => {
  it("renders user cancelled with green-tinted class", () => {
    render(<CancelCauseBadge cause={userCancelled} />);
    const el = screen.getByText(/user cancelled/i);
    expect(el).toHaveClass("cause-badge");
    expect(el).toHaveClass("cause-userCancelled");
  });

  it("renders deadline with amber-tinted class and 'deadline expired' label", () => {
    render(<CancelCauseBadge cause={deadline} />);
    const el = screen.getByText(/deadline expired/i);
    expect(el).toHaveClass("cause-deadline");
  });

  it("renders unknown with gray-tinted class", () => {
    render(<CancelCauseBadge cause={unknown} />);
    const el = screen.getByText(/cause unknown/i);
    expect(el).toHaveClass("cause-unknown");
  });

  it("accepts a custom className alongside the variant class", () => {
    render(<CancelCauseBadge cause={userCancelled} className="extra-class" />);
    const el = screen.getByText(/user cancelled/i);
    expect(el).toHaveClass("extra-class");
    expect(el).toHaveClass("cause-userCancelled");
  });
});

describe("CancelCauseDetails", () => {
  it("renders cause, confidence, source rows", () => {
    render(<CancelCauseDetails cause={userCancelled} />);
    expect(screen.getByText("cause")).toBeInTheDocument();
    expect(screen.getByText("userCancelled")).toBeInTheDocument();
    expect(screen.getByText("confidence")).toBeInTheDocument();
    expect(screen.getByText("direct")).toBeInTheDocument();
    expect(screen.getByText("source")).toBeInTheDocument();
    expect(screen.getByText("requestInterrupt")).toBeInTheDocument();
  });

  it("renders each evidence line as its own dd", () => {
    render(<CancelCauseDetails cause={userCancelled} />);
    expect(
      screen.getByText(/interrupt_requested_at = 2026-05-20T10:32:14Z/),
    ).toBeInTheDocument();
    expect(screen.getByText(/no parent cascade/)).toBeInTheDocument();
  });

  it("renders 'at' row when present", () => {
    render(<CancelCauseDetails cause={userCancelled} />);
    expect(screen.getByText("at")).toBeInTheDocument();
    expect(screen.getByText("2026-05-20T10:32:14Z")).toBeInTheDocument();
  });

  it("omits 'at' row when null", () => {
    render(<CancelCauseDetails cause={unknown} />);
    expect(screen.queryByText("at")).not.toBeInTheDocument();
  });
});
