import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../src/lib/tauri/interruptRequest", () => ({
  previewInterruptCascade: vi.fn(),
  interruptRequest: vi.fn(),
}));

import {
  interruptRequest,
  previewInterruptCascade,
} from "../src/lib/tauri/interruptRequest";
import { CascadeCancelDialog } from "../src/components/cancelUx";
import type { CascadeCancelPreview } from "../src/lib/types/operations";

const mockedPreview = vi.mocked(previewInterruptCascade);
const mockedInterrupt = vi.mocked(interruptRequest);

const examplePreview: CascadeCancelPreview = {
  rootRequestId: "req_root",
  previewSignature: "sig-1",
  rootState: "processing",
  willInterrupt: [
    {
      requestId: "req_b91",
      lifecycleState: "processing",
      parentRequestId: "req_root",
      parentToolCallId: "tc_1",
      awaitMode: "background",
      cancelPolicy: "cascade",
      toolName: "summarize",
    },
  ],
  willDetach: [],
  alreadyTerminal: [],
  unknownPolicy: [
    {
      requestId: "req_c02",
      lifecycleState: "processing",
      parentRequestId: "req_root",
      parentToolCallId: "tc_5",
      awaitMode: "background",
      cancelPolicy: null,
      toolName: "classify_docs",
    },
  ],
};

const fresherPreview: CascadeCancelPreview = {
  ...examplePreview,
  previewSignature: "sig-2",
  alreadyTerminal: [
    {
      requestId: "req_b92",
      lifecycleState: "completed",
      parentRequestId: "req_root",
      parentToolCallId: "tc_2",
      awaitMode: "background",
      cancelPolicy: "cascade",
      toolName: "index_repo",
    },
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

  it("fetches preview on open and renders the interrupt groups", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    render(<CascadeCancelDialog {...baseProps} />);

    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(mockedPreview).toHaveBeenCalledWith({
      requestId: "req_root",
      agentDid: "did:test:operator",
      includeTerminal: true,
    });
    expect(await screen.findByText(/will be interrupted/i)).toBeInTheDocument();
    expect(await screen.findByText(/no cancellation policy/i)).toBeInTheDocument();
    expect(screen.getByText(/req_b91/)).toBeInTheDocument();
    expect(screen.getByText(/req_c02/)).toBeInTheDocument();
  });

  it("confirm with accepted result calls onAccepted with timestamp and closes", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    mockedInterrupt.mockResolvedValue({
      requestId: "req_root",
      accepted: true,
      alreadyInterrupted: false,
      stalePreview: false,
      interruptRequestedAt: "2026-05-20T10:32:14Z",
    });
    render(<CascadeCancelDialog {...baseProps} />);

    fireEvent.click(
      await screen.findByRole("button", {
        name: /interrupt all/i,
      }),
    );

    await waitFor(() =>
      expect(baseProps.onAccepted).toHaveBeenCalledWith("2026-05-20T10:32:14Z"),
    );
    expect(baseProps.onClose).toHaveBeenCalled();
  });

  it("stale preview redraws and retries with the new signature", async () => {
    mockedPreview.mockResolvedValue(examplePreview);
    mockedInterrupt
      .mockResolvedValueOnce({
        requestId: "req_root",
        accepted: false,
        alreadyInterrupted: false,
        stalePreview: true,
        preview: fresherPreview,
      })
      .mockResolvedValueOnce({
        requestId: "req_root",
        accepted: true,
        alreadyInterrupted: false,
        stalePreview: false,
        interruptRequestedAt: "2026-05-20T10:32:14Z",
      });
    render(<CascadeCancelDialog {...baseProps} />);
    const confirm = await screen.findByRole("button", {
      name: /interrupt all/i,
    });

    fireEvent.click(confirm);
    await waitFor(() => expect(mockedInterrupt).toHaveBeenCalledTimes(1));
    expect(mockedInterrupt.mock.calls[0][0].expectedPreviewSignature).toBe("sig-1");
    expect(await screen.findByText(/preview updated/i)).toBeInTheDocument();
    expect(await screen.findByText(/req_b92/)).toBeInTheDocument();

    fireEvent.click(confirm);
    await waitFor(() => expect(mockedInterrupt).toHaveBeenCalledTimes(2));
    expect(mockedInterrupt.mock.calls[1][0].expectedPreviewSignature).toBe("sig-2");
    await waitFor(() =>
      expect(baseProps.onAccepted).toHaveBeenCalledWith("2026-05-20T10:32:14Z"),
    );
  });

  it("preview fetch error calls onError and onClose", async () => {
    mockedPreview.mockRejectedValue("bridge offline");
    render(<CascadeCancelDialog {...baseProps} />);

    await waitFor(() => {
      expect(baseProps.onError).toHaveBeenCalledWith(
        expect.stringContaining("bridge offline"),
      );
    });
    expect(baseProps.onClose).toHaveBeenCalled();
  });
});
