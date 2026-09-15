import { act, renderHook } from "@testing-library/react";
import { useRef, useState } from "react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { createDesktopShellPeerActions } from "../src/hooks/desktopShellPeerActions";
import { useDesktopMailboxRoute } from "../src/hooks/useDesktopMailboxRoute";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

function usePeerRoute(api: DesktopApiAdapter) {
  const [agent, setAgent] = useState<string | null>("agent-a");
  const [behavior, setBehavior] = useState<string | null>("behavior-a");
  const [sessionId, setSessionId] = useState<string | null>("session-a");
  const [, setSession] = useState<DesktopSessionSnapshot | null>(null);
  const selectedAgentDidRef = useRef(agent);
  selectedAgentDidRef.current = agent;
  const route = useDesktopMailboxRoute({
    api,
    refreshSnapshot: async () => {},
    selectedAgentDid: agent,
    selectedBehaviorId: behavior,
    selectedSessionId: sessionId,
    setError: vi.fn(),
    setSelectedAgentDid: setAgent,
    setSelectedBehaviorId: setBehavior,
    setSelectedSessionId: setSessionId,
    setSession,
  });
  const actions = createDesktopShellPeerActions({
    api,
    ensureDesktopClientStarted: async () => ({ client: {} }) as DesktopClientSnapshot,
    mutateSnapshot: async <T,>(operation: () => Promise<T>) => operation(),
    refreshSnapshot: async () => {},
    selectedAgentDidRef,
    selectAgent: route.selectAgent,
    setAddingPeer: vi.fn(),
    setError: vi.fn(),
    setRepairingP2P: vi.fn(),
    setStarting: vi.fn(),
    snapshot: null,
  });
  return { actions, agent, behavior, route, sessionId };
}

describe("peer action route ownership", () => {
  it("selects a newly provisioned local agent through the route owner", async () => {
    const api = {
      initLocalStandardRuntime: vi.fn(async () => ({ agentDid: "agent-local" })),
    } as unknown as DesktopApiAdapter;
    const { result } = renderHook(() => usePeerRoute(api));

    await act(async () => result.current.actions.onInitLocalRuntime("Local"));

    expect(result.current.agent).toBe("agent-local");
    expect(result.current.sessionId).toBeNull();
    expect(result.current.behavior).toBeNull();
  });

  it("does not clear newer navigation when removal completes late", async () => {
    const pending = deferred<DesktopClientSnapshot>();
    const api = {
      removePeer: vi.fn(() => pending.promise),
    } as unknown as DesktopApiAdapter;
    const { result } = renderHook(() => usePeerRoute(api));

    const completion = result.current.actions.onRemovePeer("peer-a", "agent-a");
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

    await act(async () => result.current.actions.onRemovePeer("peer-a", "agent-a"));

    expect(result.current.agent).toBeNull();
    expect(result.current.sessionId).toBeNull();
    expect(result.current.behavior).toBeNull();
  });
});
