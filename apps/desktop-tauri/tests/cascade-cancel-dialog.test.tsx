import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";

vi.mock("../src/lib/tauri/interruptRequest", () => ({
  previewInterruptCascade: vi.fn(),
  interruptRequest: vi.fn(),
}));

import { previewInterruptCascade, interruptRequest } from "../src/lib/tauri/interruptRequest";
import { CascadeCancelDialog } from "../src/components/cancelUx";
import type { CascadeCancelPreview } from "../src/lib/types/operations";

const mockedPreview = vi.mocked(previewInterruptCascade);
const mockedInterrupt = vi.mocked(interruptRequest);

const examplePreview: CascadeCancelPreview = {
  rootRequestId: "req_root",
  previewSignature: "sig-1",
  rootState: "processing",
  willInterrupt: [
    { requestId: "req_b91", lifecycleState: "processing", parentRequestId: "req_root", parentToolCallId: "tc_1", awaitMode: "background", cancelPolicy: "cascade", toolName: "summarize" },
  ],
  willDetach: [],
  alreadyTerminal: [],
  unknownPolicy: [
    { requestId: "req_c02", lifecycleState: "processing", parentRequestId: "req_root", parentToolCallId: "tc_5", awaitMode: "background", cancelPolicy: null, toolName: "classify_docs" },
  ],
};

const fresherPreview: CascadeCancelPreview = {
  ...examplePreview,
  previewSignature: "sig-2",
  alreadyTerminal: [
    { requestId: "req_b92", lifecycleState: "completed", parentRequestId: "req_root", parentToolCallId: "tc_2", awaitMode: "background", cancelPolicy: "cascade", toolName: "index_repo" },
  ],
};

const baseProps = {
  open: true,
  rootRequestId: "req_root",
  agentDid: "did:test:operator",
  onClose: vi.fn(),
  onAccepted: vi.fn(),
  onAlreadyInterrupted: vi.fn(),
  onError: vi.fn(),
};

describe("CascadeCancelDialog", () => {
  beforeEach(() => {
    mockedPreview.mockReset();
    mockedInterrupt.mockReset();
    baseProps.onClose.mockReset();
    baseProps.onAccepted.mockReset();
    baseProps.onAlreadyInterrupted.mockReset();
    baseProps.onError.mockReset();
  });

  it("renders nothing when open is false", () => {
    const { container } = render(<CascadeCancelDialog {...baseProps} open={false} />);
    expect(container.firstChild).toBeNull();
  });

  it("fetches preview on open and renders all four groups (only non-empty ones)", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    render(<CascadeCancelDialog {...baseProps} />);
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(mockedPreview).toHaveBeenCalledWith({
      requestId: "req_root", agentDid: "did:test:operator", includeTerminal: true,
    });
    expect(await screen.findByText(/will request interrupt by cascade/i)).toBeInTheDocument();
    expect(await screen.findByText(/policy unknown/i)).toBeInTheDocument();
    expect(screen.getByText(/req_b91/)).toBeInTheDocument();
    expect(screen.getByText(/req_c02/)).toBeInTheDocument();
  });

  it("dialog has role=dialog with aria-modal", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    render(<CascadeCancelDialog {...baseProps} />);
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveAttribute("aria-modal", "true");
  });

  it("clicking Cancel calls onClose without latching the interrupt", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    render(<CascadeCancelDialog {...baseProps} />);
    const cancel = await screen.findByRole("button", { name: /^cancel$/i });
    fireEvent.click(cancel);
    expect(baseProps.onClose).toHaveBeenCalled();
    expect(mockedInterrupt).not.toHaveBeenCalled();
  });

  it("ESC closes and calls onClose", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    render(<CascadeCancelDialog {...baseProps} />);
    await screen.findByRole("dialog");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(baseProps.onClose).toHaveBeenCalled();
  });

  it("confirm with accepted result calls onAccepted with timestamp and closes", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    mockedInterrupt.mockResolvedValue({
      requestId: "req_root", accepted: true, alreadyInterrupted: false,
      stalePreview: false, interruptRequestedAt: "2026-05-20T10:32:14Z",
    });
    render(<CascadeCancelDialog {...baseProps} />);
    const confirm = await screen.findByRole("button", { name: /interrupt parent and cascade/i });
    fireEvent.click(confirm);
    await waitFor(() => expect(baseProps.onAccepted).toHaveBeenCalledWith("2026-05-20T10:32:14Z"));
    expect(baseProps.onClose).toHaveBeenCalled();
  });

  it("confirm with alreadyInterrupted result calls onAlreadyInterrupted and closes", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    mockedInterrupt.mockResolvedValue({
      requestId: "req_root", accepted: false, alreadyInterrupted: true, stalePreview: false,
    });
    render(<CascadeCancelDialog {...baseProps} />);
    const confirm = await screen.findByRole("button", { name: /interrupt parent and cascade/i });
    fireEvent.click(confirm);
    await waitFor(() => expect(baseProps.onAlreadyInterrupted).toHaveBeenCalled());
    expect(baseProps.onClose).toHaveBeenCalled();
  });

  it("confirm with stalePreview redraws with new signature and shows preview-updated pill; second confirm uses new signature", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    mockedInterrupt
      .mockResolvedValueOnce({
        requestId: "req_root", accepted: false, alreadyInterrupted: false,
        stalePreview: true, preview: fresherPreview,
      })
      .mockResolvedValueOnce({
        requestId: "req_root", accepted: true, alreadyInterrupted: false,
        stalePreview: false, interruptRequestedAt: "2026-05-20T10:32:14Z",
      });
    render(<CascadeCancelDialog {...baseProps} />);
    const confirm = await screen.findByRole("button", { name: /interrupt parent and cascade/i });

    // First confirm with sig-1
    fireEvent.click(confirm);
    await waitFor(() => expect(mockedInterrupt).toHaveBeenCalledTimes(1));
    expect(mockedInterrupt.mock.calls[0][0].expectedPreviewSignature).toBe("sig-1");

    // After stalePreview: pill appears, dialog stays open
    expect(await screen.findByText(/preview updated/i)).toBeInTheDocument();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(baseProps.onAccepted).not.toHaveBeenCalled();

    // The fresher preview's alreadyTerminal entry should now show
    expect(await screen.findByText(/req_b92/)).toBeInTheDocument();

    // Second confirm uses sig-2
    fireEvent.click(confirm);
    await waitFor(() => expect(mockedInterrupt).toHaveBeenCalledTimes(2));
    expect(mockedInterrupt.mock.calls[1][0].expectedPreviewSignature).toBe("sig-2");
    await waitFor(() => expect(baseProps.onAccepted).toHaveBeenCalledWith("2026-05-20T10:32:14Z"));
  });

  it("focus is on Cancel button when dialog opens", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    render(<CascadeCancelDialog {...baseProps} />);
    const cancel = await screen.findByRole("button", { name: /^cancel$/i });
    await waitFor(() => expect(cancel).toHaveFocus());
  });

  it("Tab from Confirm cycles back to Cancel (focus trap)", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    render(<CascadeCancelDialog {...baseProps} />);
    const cancel = await screen.findByRole("button", { name: /^cancel$/i });
    const confirm = await screen.findByRole("button", { name: /interrupt parent and cascade/i });

    confirm.focus();
    expect(confirm).toHaveFocus();

    fireEvent.keyDown(confirm, { key: "Tab" });
    expect(cancel).toHaveFocus();
  });

  it("Shift+Tab from Cancel cycles forward to Confirm (focus trap)", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    render(<CascadeCancelDialog {...baseProps} />);
    const cancel = await screen.findByRole("button", { name: /^cancel$/i });
    const confirm = await screen.findByRole("button", { name: /interrupt parent and cascade/i });

    cancel.focus();
    fireEvent.keyDown(cancel, { key: "Tab", shiftKey: true });
    expect(confirm).toHaveFocus();
  });

  it("preview fetch error calls onError and onClose", async () => {
    mockedPreview.mockRejectedValue("bridge offline");
    render(<CascadeCancelDialog {...baseProps} />);
    await waitFor(() => {
      expect(baseProps.onError).toHaveBeenCalledWith(expect.stringContaining("bridge offline"));
    });
    expect(baseProps.onClose).toHaveBeenCalled();
  });
});
