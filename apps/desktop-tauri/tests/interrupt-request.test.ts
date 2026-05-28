import { describe, expect, it, vi, beforeEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import {
  interruptRequest,
  previewInterruptCascade,
} from "../src/lib/tauri/interruptRequest";

const mockedInvoke = vi.mocked(invoke);

// Simulate the Tauri desktop bridge being available so that invokeDesktop()
// passes its hasTauriInvokeBridge() guard and reaches the mocked invoke.
beforeEach(() => {
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    value: { invoke: mockedInvoke },
    configurable: true,
    writable: true,
  });
});

describe("previewInterruptCascade", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });
  it("calls desktop_preview_interrupt_cascade with the request wrapped under {request}", async () => {
    mockedInvoke.mockResolvedValue({
      rootRequestId: "req_root",
      previewSignature: "abc",
      willInterrupt: [],
      willDetach: [],
      alreadyTerminal: [],
      unknownPolicy: [],
    });
    const result = await previewInterruptCascade({
      requestId: "req_root",
      agentDid: "did:test:op",
      includeTerminal: true,
    });
    expect(mockedInvoke).toHaveBeenCalledWith("desktop_preview_interrupt_cascade", {
      request: {
        requestId: "req_root",
        agentDid: "did:test:op",
        includeTerminal: true,
      },
    });
    expect(result.rootRequestId).toBe("req_root");
  });
});

describe("interruptRequest", () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });
  it("calls desktop_interrupt_request with the request wrapped under {request}", async () => {
    mockedInvoke.mockResolvedValue({
      requestId: "req_root",
      accepted: true,
      alreadyInterrupted: false,
      stalePreview: false,
      interruptRequestedAt: "2026-05-20T10:32:14Z",
    });
    const result = await interruptRequest({
      requestId: "req_root",
      cause: "userCancelled",
      cascade: false,
    });
    expect(mockedInvoke).toHaveBeenCalledWith("desktop_interrupt_request", {
      request: { requestId: "req_root", cause: "userCancelled", cascade: false },
    });
    expect(result.accepted).toBe(true);
  });

  it("passes expectedPreviewSignature on cascade calls", async () => {
    mockedInvoke.mockResolvedValue({
      requestId: "req_root",
      accepted: true,
      alreadyInterrupted: false,
      stalePreview: false,
    });
    await interruptRequest({
      requestId: "req_root",
      cause: "userCancelled",
      cascade: true,
      expectedPreviewSignature: "sig123",
    });
    expect(mockedInvoke).toHaveBeenCalledWith("desktop_interrupt_request", {
      request: {
        requestId: "req_root",
        cause: "userCancelled",
        cascade: true,
        expectedPreviewSignature: "sig123",
      },
    });
  });
});
