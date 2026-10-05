import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ShellBridge } from "../src/ui/hooks/useShell";

const desktop = vi.hoisted(() => ({
  state: {} as Record<string, unknown>,
}));
vi.mock("../src/hooks/useDesktopShell", () => ({
  useDesktopShell: () => desktop.state,
}));

import { useShell } from "../src/ui/hooks/useShell";

const node = (agentDid: string, sessionIds: string[]) => ({
  agentDid,
  behaviors: [],
  sessions: sessionIds.map((sessionId) => ({ sessionId })),
});

beforeEach(() => {
  desktop.state = {
    deployments: [node("a", ["a-1"]), node("b", ["b-1"])],
    selectedAgentDid: "a",
    selectedSessionId: "a-1",
    pendingMailboxCauseId: null,
    setSelectedAgentDid: vi.fn(),
    onSelectSession: vi.fn(),
    onStartNewSession: vi.fn(),
    behaviorReadiness: {},
  };
});

const bridge = { api: {} } as unknown as ShellBridge;

describe("useShell route selection", () => {
  it("selects the session's node first, then the session once, against it", () => {
    const { rerender } = renderHook(({ route }) => useShell(bridge, route), {
      initialProps: { route: "a-1" as string | null | undefined },
    });
    rerender({ route: "b-1" });
    expect(desktop.state.setSelectedAgentDid).toHaveBeenCalledWith("b");
    expect(desktop.state.onSelectSession).not.toHaveBeenCalled();
    desktop.state = {
      ...desktop.state,
      selectedAgentDid: "b",
      selectedSessionId: null,
    };
    rerender({ route: "b-1" });
    expect(desktop.state.onSelectSession).toHaveBeenCalledTimes(1);
    expect(desktop.state.onSelectSession).toHaveBeenCalledWith("b-1");
  });

  it("selects a session on the selected node without touching the node", () => {
    const { rerender } = renderHook(({ route }) => useShell(bridge, route), {
      initialProps: { route: undefined as string | null | undefined },
    });
    desktop.state = { ...desktop.state, selectedSessionId: null };
    rerender({ route: "a-1" });
    expect(desktop.state.setSelectedAgentDid).not.toHaveBeenCalled();
    expect(desktop.state.onSelectSession).toHaveBeenCalledTimes(1);
  });

  it("keeps a mailbox item's cause when it opens into a new session", () => {
    desktop.state = {
      ...desktop.state,
      selectedSessionId: null,
      pendingMailboxCauseId: "item-1",
    };
    const { rerender } = renderHook(({ route }) => useShell(bridge, route), {
      initialProps: { route: undefined as string | null | undefined },
    });
    rerender({ route: null });
    expect(desktop.state.onStartNewSession).not.toHaveBeenCalled();
  });

  it("starts a new session for an ordinary new-session route", () => {
    const { rerender } = renderHook(({ route }) => useShell(bridge, route), {
      initialProps: { route: undefined as string | null | undefined },
    });
    rerender({ route: null });
    expect(desktop.state.onStartNewSession).toHaveBeenCalledTimes(1);
  });
});
