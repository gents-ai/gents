import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RequestTracePanel } from "../src/components/trace/RequestTracePanel";
import {
  eventSummary,
  eventTimestamp,
} from "../src/components/trace/RequestTracePanel";
import { setDesktopApiAdapterForTests } from "../src/lib/desktop-api";
import type { DesktopApiAdapter } from "../src/lib/desktop-api";

function adapterWith(timeline: unknown, fail = false) {
  return {
    fetchRequestTimeline: fail
      ? vi.fn().mockRejectedValue(new Error("peer unreachable"))
      : vi.fn().mockResolvedValue(timeline),
  } as unknown as DesktopApiAdapter;
}

describe("request trace panel", () => {
  afterEach(() => setDesktopApiAdapterForTests(null));

  it("renders the reconstructed event stream", async () => {
    setDesktopApiAdapterForTests(
      adapterWith({
        request_id: "req-1",
        events: [
          {
            kind: "message",
            role: "user",
            content: "hi",
            timestamp: "2026-06-03T14:05:00Z",
          },
          { kind: "tool_call", tool_name: "gents_exec", lifecycle_state: "Completed" },
          { kind: "response", status: "materialized" },
        ],
      }),
    );
    render(<RequestTracePanel agentDid="did:a" rootRequestId="req-1" />);

    await waitFor(() => expect(screen.getByText("user: hi")).toBeInTheDocument());
    expect(screen.getByText("gents_exec — Completed")).toBeInTheDocument();
    expect(screen.getByText("materialized")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy JSON" })).toBeInTheDocument();
  });

  it("surfaces fetch failures with a retry affordance", async () => {
    setDesktopApiAdapterForTests(adapterWith(null, true));
    render(<RequestTracePanel agentDid="did:a" rootRequestId="req-1" />);

    await waitFor(() =>
      expect(screen.getByTestId("trace-error")).toHaveTextContent("peer unreachable"),
    );
    expect(screen.getByTestId("trace-refresh")).toBeEnabled();
  });

  it("asks for a request when none is selected", () => {
    render(<RequestTracePanel agentDid="did:a" rootRequestId={null} />);
    expect(screen.getByText(/No request selected/)).toBeInTheDocument();
  });

  it("summarizes and timestamps each event kind honestly", () => {
    expect(
      eventSummary({
        kind: "inference_call",
        call_seq: 2,
        call_state: "completed",
        backend_id: "b1",
      }),
    ).toBe("call #2 — completed — b1");
    expect(
      eventSummary({
        kind: "request",
        lifecycle_state: "Failed",
        failure_reason: "boom",
      }),
    ).toBe("Failed — boom");
    expect(
      eventTimestamp({ kind: "tool_call", started_at: "2026-01-01T00:00:00Z" }),
    ).toBe("2026-01-01T00:00:00Z");
    expect(eventTimestamp({ kind: "response" })).toBeNull();
  });
});
