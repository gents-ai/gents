import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { HoldsPanel } from "@source-inc/gents-desktop-operations";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { HeldToolCallView } from "@source-inc/gents-desktop-client";

const AGENT_DID = "did:key:z6MkHoldsAgent";

function heldCall(overrides: Partial<HeldToolCallView> = {}): HeldToolCallView {
  return {
    toolCallId: "call-1",
    requestId: "req-1",
    sessionId: "session-1",
    agentDid: AGENT_DID,
    toolName: "bash_unrestricted",
    args: '{"command":"cargo publish"}',
    deadlineAt: new Date(Date.now() + 5 * 60_000).toISOString(),
    ...overrides,
  };
}

function withAdapter(overrides: Partial<DesktopApiAdapter>) {
  return overrides as unknown as DesktopApiAdapter;
}

describe("holds panel", () => {
  it("lists held calls with tool name, args preview, and request id", async () => {
    const api = withAdapter({
      listToolCallHolds: vi.fn().mockResolvedValue([heldCall()]),
      resolveToolCallHold: vi.fn(),
    });
    render(<HoldsPanel agentDid={AGENT_DID} api={api} />);

    await waitFor(() =>
      expect(screen.getByTestId("hold-row-call-1")).toBeInTheDocument(),
    );
    expect(screen.getByText("bash_unrestricted")).toBeInTheDocument();
    expect(screen.getByTestId("hold-args-preview-call-1")).toHaveTextContent(
      '{"command":"cargo publish"}',
    );
    expect(screen.getByText("request req-1")).toBeInTheDocument();
  });

  it("expands the complete args when the significant tail is past the preview", async () => {
    const significantTail = "rm -rf /srv/production";
    const longArgs = JSON.stringify({
      command: `${"echo safe && ".repeat(12)}${significantTail}`,
    });
    expect(longArgs.length).toBeGreaterThan(120);
    const api = withAdapter({
      listToolCallHolds: vi.fn().mockResolvedValue([heldCall({ args: longArgs })]),
      resolveToolCallHold: vi.fn(),
    });
    render(<HoldsPanel agentDid={AGENT_DID} api={api} />);

    await waitFor(() =>
      expect(screen.getByTestId("hold-row-call-1")).toBeInTheDocument(),
    );
    expect(screen.getByTestId("hold-args-preview-call-1")).not.toHaveTextContent(
      significantTail,
    );
    expect(screen.getByTestId("hold-args-details-call-1")).not.toHaveAttribute("open");

    fireEvent.click(screen.getByTestId("hold-args-toggle-call-1"));

    expect(screen.getByTestId("hold-args-details-call-1")).toHaveAttribute("open");
    expect(screen.getByTestId("hold-args-full-call-1")).toHaveTextContent(
      significantTail,
    );
    expect(screen.getByTestId("hold-args-full-call-1")).toHaveTextContent(longArgs);
  });

  it("shows the empty state when nothing is held", async () => {
    const api = withAdapter({
      listToolCallHolds: vi.fn().mockResolvedValue([]),
      resolveToolCallHold: vi.fn(),
    });
    render(<HoldsPanel agentDid={AGENT_DID} api={api} />);

    await waitFor(() => expect(screen.getByTestId("holds-empty")).toBeInTheDocument());
  });

  it("can stay out of the chat surface when no approval is pending", async () => {
    const api = withAdapter({
      listToolCallHolds: vi.fn().mockResolvedValue([]),
      resolveToolCallHold: vi.fn(),
    });
    render(<HoldsPanel agentDid={AGENT_DID} api={api} hideWhenIdle />);

    expect(screen.queryByTestId("holds-panel")).not.toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByTestId("holds-panel")).not.toBeInTheDocument(),
    );
  });

  it("approves a held call and refreshes the list", async () => {
    const resolveToolCallHold = vi.fn().mockResolvedValue({
      approvalId: "approval-call-1-x",
      toolCallId: "call-1",
      decision: "approved",
    });
    const listToolCallHolds = vi
      .fn()
      .mockResolvedValueOnce([heldCall()])
      .mockResolvedValue([]);
    const api = withAdapter({ listToolCallHolds, resolveToolCallHold });
    render(<HoldsPanel agentDid={AGENT_DID} api={api} />);

    await waitFor(() =>
      expect(screen.getByTestId("hold-approve-call-1")).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByTestId("hold-approve-call-1"));

    await waitFor(() =>
      expect(resolveToolCallHold).toHaveBeenCalledWith({
        agentDid: AGENT_DID,
        toolCallId: "call-1",
        approve: true,
        reason: null,
      }),
    );
    await waitFor(() => expect(screen.getByTestId("holds-empty")).toBeInTheDocument());
  });

  it("denies with a reason after confirmation", async () => {
    const resolveToolCallHold = vi.fn().mockResolvedValue({
      approvalId: "approval-call-1-y",
      toolCallId: "call-1",
      decision: "denied",
    });
    const listToolCallHolds = vi
      .fn()
      .mockResolvedValueOnce([heldCall()])
      .mockResolvedValue([]);
    const api = withAdapter({ listToolCallHolds, resolveToolCallHold });
    render(<HoldsPanel agentDid={AGENT_DID} api={api} />);

    await waitFor(() =>
      expect(screen.getByTestId("hold-deny-call-1")).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByTestId("hold-deny-call-1"));
    fireEvent.change(screen.getByTestId("hold-deny-reason-call-1"), {
      target: { value: "not on prod" },
    });
    fireEvent.click(screen.getByTestId("hold-deny-confirm-call-1"));

    await waitFor(() =>
      expect(resolveToolCallHold).toHaveBeenCalledWith({
        agentDid: AGENT_DID,
        toolCallId: "call-1",
        approve: false,
        reason: "not on prod",
      }),
    );
  });

  it("surfaces list errors and resolve errors separately", async () => {
    const api = withAdapter({
      listToolCallHolds: vi.fn().mockRejectedValue(new Error("bridge offline")),
      resolveToolCallHold: vi.fn(),
    });
    render(<HoldsPanel agentDid={AGENT_DID} api={api} />);

    await waitFor(() =>
      expect(screen.getByTestId("holds-error")).toHaveTextContent("bridge offline"),
    );
  });

  it("shows a resolve failure without dropping the row", async () => {
    const listToolCallHolds = vi.fn().mockResolvedValue([heldCall()]);
    const resolveToolCallHold = vi
      .fn()
      .mockRejectedValue(new Error("hold already resolved"));
    const api = withAdapter({ listToolCallHolds, resolveToolCallHold });
    render(<HoldsPanel agentDid={AGENT_DID} api={api} />);

    await waitFor(() =>
      expect(screen.getByTestId("hold-approve-call-1")).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByTestId("hold-approve-call-1"));

    await waitFor(() =>
      expect(screen.getByTestId("holds-action-error")).toHaveTextContent(
        "hold already resolved",
      ),
    );
    expect(screen.getByTestId("hold-row-call-1")).toBeInTheDocument();
  });
});
