import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HoldsPanel } from "../src/components/operations/HoldsPanel";
import { setDesktopApiAdapterForTests } from "../src/lib/desktop-api";
import type { DesktopApiAdapter } from "../src/lib/desktop-api";
import type { HeldToolCallView } from "../src/lib/types/operations";

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
  setDesktopApiAdapterForTests(overrides as unknown as DesktopApiAdapter);
}

describe("holds panel", () => {
  afterEach(() => setDesktopApiAdapterForTests(null));

  it("lists held calls with tool name, args preview, and request id", async () => {
    withAdapter({
      listToolCallHolds: vi.fn().mockResolvedValue([heldCall()]),
      resolveToolCallHold: vi.fn(),
    });
    render(<HoldsPanel agentDid={AGENT_DID} />);

    await waitFor(() =>
      expect(screen.getByTestId("hold-row-call-1")).toBeInTheDocument(),
    );
    expect(screen.getByText("bash_unrestricted")).toBeInTheDocument();
    expect(screen.getByText('{"command":"cargo publish"}')).toBeInTheDocument();
    expect(screen.getByText("request req-1")).toBeInTheDocument();
  });

  it("shows the empty state when nothing is held", async () => {
    withAdapter({
      listToolCallHolds: vi.fn().mockResolvedValue([]),
      resolveToolCallHold: vi.fn(),
    });
    render(<HoldsPanel agentDid={AGENT_DID} />);

    await waitFor(() => expect(screen.getByTestId("holds-empty")).toBeInTheDocument());
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
    withAdapter({ listToolCallHolds, resolveToolCallHold });
    render(<HoldsPanel agentDid={AGENT_DID} />);

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
    withAdapter({ listToolCallHolds, resolveToolCallHold });
    render(<HoldsPanel agentDid={AGENT_DID} />);

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
    withAdapter({
      listToolCallHolds: vi.fn().mockRejectedValue(new Error("bridge offline")),
      resolveToolCallHold: vi.fn(),
    });
    render(<HoldsPanel agentDid={AGENT_DID} />);

    await waitFor(() =>
      expect(screen.getByTestId("holds-error")).toHaveTextContent("bridge offline"),
    );
  });

  it("shows a resolve failure without dropping the row", async () => {
    const listToolCallHolds = vi.fn().mockResolvedValue([heldCall()]);
    const resolveToolCallHold = vi
      .fn()
      .mockRejectedValue(new Error("hold already resolved"));
    withAdapter({ listToolCallHolds, resolveToolCallHold });
    render(<HoldsPanel agentDid={AGENT_DID} />);

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
