import { act, renderHook } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { createPeerActions } from "../src/hooks/peerActions";
import { createSelectionActions } from "../src/hooks/selectionActions";
import { useSelection } from "../src/hooks/selectionStore";
import { shellStores } from "./shell-fixture";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

function usePeerRoute(
  api: DesktopApiAdapter,
  ensureDesktopClientStarted = async () => ({ client: {} }) as DesktopClientSnapshot,
) {
  const [stores] = useState(() =>
    shellStores({
      selection: {
        agentDid: "agent-a",
        behaviorId: "behavior-a",
        sessionId: "session-a",
      },
    }),
  );
  const current = useSelection(stores.selection);
  const [route] = useState(() => createSelectionActions({ stores }));
  const [actions] = useState(() =>
    createPeerActions({
      api,
      stores,
      ensureDesktopClientStarted,
      mutateSnapshot: async <T,>(operation: () => Promise<T>) => operation(),
      refreshSnapshot: async () => {},
      selectAgent: route.selectAgent,
      reportFailure: vi.fn(),
    }),
  );
  return {
    actions,
    agent: current.agentDid,
    behavior: current.behaviorId,
    route,
    sessionId: current.sessionId,
  };
}

describe("peer action route ownership", () => {
  it("selects a newly provisioned local agent through the route owner", async () => {
    const api = {
      initLocalStandardRuntime: vi.fn(async () => ({ agentDid: "agent-local" })),
    } as unknown as DesktopApiAdapter;
    const { result } = renderHook(() => usePeerRoute(api));

    await act(async () => result.current.actions.initLocalRuntime("Local"));

    expect(result.current.agent).toBe("agent-local");
    expect(result.current.sessionId).toBeNull();
    expect(result.current.behavior).toBeNull();
  });

  it("preserves the exact client-start failure after local runtime init", async () => {
    const api = {
      initLocalStandardRuntime: vi.fn(async () => ({ agentDid: "agent-local" })),
    } as unknown as DesktopApiAdapter;
    const startError = new Error("opening desktop identity key: permission denied");
    const { result } = renderHook(() =>
      usePeerRoute(api, async () => {
        throw startError;
      }),
    );

    await expect(
      act(async () => result.current.actions.initLocalRuntime("Local")),
    ).rejects.toThrow("opening desktop identity key: permission denied");
  });

  it("does not clear newer navigation when removal completes late", async () => {
    const pending = deferred<DesktopClientSnapshot>();
    const api = {
      removePeer: vi.fn(() => pending.promise),
    } as unknown as DesktopApiAdapter;
    const { result } = renderHook(() => usePeerRoute(api));

    const completion = result.current.actions.removePeer("peer-a", "agent-a");
    act(() => result.current.route.selectAgent("agent-b"));
    await act(async () => {
      pending.resolve({} as DesktopClientSnapshot);
      await completion;
    });

    expect(result.current.agent).toBe("agent-b");
  });

  it("clears the route when its selected peer is removed", async () => {
    const api = {
      removePeer: vi.fn(async () => ({})),
    } as unknown as DesktopApiAdapter;
    const { result } = renderHook(() => usePeerRoute(api));

    await act(async () => result.current.actions.removePeer("peer-a", "agent-a"));

    expect(result.current.agent).toBeNull();
    expect(result.current.sessionId).toBeNull();
    expect(result.current.behavior).toBeNull();
  });
});

describe("enrolment and peer status leave failures to the screen", () => {
  it("throws an enrolment failure in the peer's words without reporting it", async () => {
    const reportFailure = vi.fn();
    const refreshSnapshot = vi.fn(async () => {});
    const actions = createPeerActions({
      api: {
        requestStatusEnrollment: vi.fn().mockRejectedValue(new Error("server refused")),
      } as unknown as DesktopApiAdapter,
      stores: shellStores(),
      ensureDesktopClientStarted: async () => ({ client: {} }) as DesktopClientSnapshot,
      mutateSnapshot: async <T,>(operation: () => Promise<T>) => operation(),
      refreshSnapshot,
      selectAgent: vi.fn(),
      reportFailure,
    });

    await expect(actions.requestStatusEnrollment("gents.example:7777")).rejects.toThrow(
      "server refused",
    );
    expect(reportFailure).not.toHaveBeenCalled();
    expect(refreshSnapshot).not.toHaveBeenCalled();
  });
});
