import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@source-inc/gents-desktop-client", () => ({
  fetchOperationsSnapshot: vi.fn(),
}));

import { useOperationsSnapshot } from "@source-inc/gents-desktop-operations";
import { fetchOperationsSnapshot } from "@source-inc/gents-desktop-client";
import type { DesktopOperationsSnapshot } from "@source-inc/gents-desktop-client";

const mockedFetch = vi.mocked(fetchOperationsSnapshot);

function snapshot(agentDid: string): DesktopOperationsSnapshot {
  return {
    fetchedAt: "2026-07-17T00:00:00Z",
    agentDid,
    backgroundedTools: [],
    stuckDiagnostics: [],
  };
}

describe("useOperationsSnapshot", () => {
  beforeEach(() => {
    mockedFetch.mockReset();
  });

  it("does not poll while disabled", async () => {
    const { result } = renderHook(() =>
      useOperationsSnapshot({ agentDid: "did:key:z6MkA" }, { enabled: false }),
    );

    await act(async () => Promise.resolve());
    expect(mockedFetch).not.toHaveBeenCalled();
    expect(result.current.snapshot).toBeNull();
  });

  it("does not expose the prior deployment snapshot during a switch", async () => {
    let resolveSecond: ((value: DesktopOperationsSnapshot) => void) | undefined;
    mockedFetch.mockImplementation(async (request) => {
      if (request.agentDid === "did:key:z6MkA") return snapshot("did:key:z6MkA");
      return new Promise((resolve) => {
        resolveSecond = resolve;
      });
    });

    const { result, rerender } = renderHook(
      ({ agentDid }) => useOperationsSnapshot({ agentDid }),
      { initialProps: { agentDid: "did:key:z6MkA" } },
    );
    await waitFor(() =>
      expect(result.current.snapshot?.agentDid).toBe("did:key:z6MkA"),
    );

    rerender({ agentDid: "did:key:z6MkB" });
    expect(result.current.snapshot).toBeNull();
    expect(result.current.isLoading).toBe(true);

    await act(async () => {
      resolveSecond?.(snapshot("did:key:z6MkB"));
    });
    await waitFor(() =>
      expect(result.current.snapshot?.agentDid).toBe("did:key:z6MkB"),
    );
  });
});
