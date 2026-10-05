import { act, renderHook } from "@testing-library/react";
import { useRef, useState } from "react";
import { describe, expect, it, vi } from "vitest";
import type {
  DeploymentView,
  DesktopApiAdapter,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import { useDesktopShellEffects } from "../src/hooks/desktopShellEffects";
import { createDesktopShellSelectionActions } from "../src/hooks/desktopShellSelectionActions";
import { createSelectionStore, useSelection } from "../src/hooks/selectionStore";
import { createDesktopShellChatActions } from "../src/hooks/desktopShellChatActions";

const initialDeployment = {
  agentDid: "agent",
  agentPrincipal: { defaultBehaviorId: "coding" },
  behaviors: [
    { behaviorId: "coding", enabled: true, isDefault: true },
    { behaviorId: "setup", enabled: true, isDefault: false },
  ],
  sessions: [
    { sessionId: "first-setup", behaviorId: "setup" },
    { sessionId: "first-coding", behaviorId: "coding" },
  ],
} as unknown as DeploymentView;

function useHarness(
  deployment: DeploymentView | null,
  initialSession: string | null,
  initialAgent: string | null = "agent",
) {
  const [store] = useState(() =>
    createSelectionStore({
      agentDid: initialAgent,
      behaviorId: "setup",
      sessionId: initialSession,
    }),
  );
  const current = useSelection(store);
  const [workflow, setWorkflow] = useState<ChatWorkflowState>({ kind: "ready" });
  const [, setSession] = useState<DesktopSessionSnapshot | null>(null);
  const sendChatMessage = useRef(
    vi.fn(async () => ({
      agentDid: "agent",
      behaviorId: "setup",
      sessionId: "new-setup",
      requestId: "new-request",
    })),
  ).current;
  const api = useRef({ sendChatMessage }).current as unknown as DesktopApiAdapter;
  const ref = useRef({ current: null }).current;
  const deployments = deployment ? [deployment] : [];
  const route = createDesktopShellSelectionActions({
    store,
    deployments: () => deployments,
    setSession,
    setLocalWorkflow: setWorkflow,
    setError: vi.fn(),
  });
  const actions = {
    ...route,
    ...createDesktopShellChatActions({
      submissionInFlight: useRef(false),
      store,
      api,
      draft: "",
      selectedDeployment:
        deployments.find((d) => d.agentDid === current.agentDid) ?? null,
      deployments,
      behaviorReadiness: { kind: "ready", behaviorId: "setup", behaviorLabel: "Setup" },
      refreshSession: async () => null,
      refreshSnapshot: async () => {},
      setDraft: vi.fn(),
      setError: vi.fn(),
      setLocalWorkflow: setWorkflow,
      setOptimisticPendingTurn: vi.fn(),
      setSending: vi.fn(),
      shellProjection: { nonEmptyContentSendStatus: { kind: "ready" } } as never,
      retryShellProjection: {} as never,
    }),
  };
  useDesktopShellEffects({
    api,
    autoRestartInFlight: { current: false },
    autostartAttempted: { current: true },
    deployments,
    lastObservedP2PHealth: ref,
    lastP2PAutoRestartAt: ref,
    localWorkflow: workflow,
    clientAutostarts: () => false,
    listenToUpdates: async () => () => {},
    composingFor: current.composingFor,
    refreshSession: async () => null,
    refreshSessionLiveDelta: async () => false,
    refreshSnapshot: async () => {},
    restartDesktopClient: async () => {},
    runtimeHealth: null,
    selectedAgentDid: current.agentDid,
    selectedBehaviorId: current.behaviorId,
    selectedDeployment: deployment,
    selectedSessionId: current.sessionId,
    store,
    selectedTrackedRequestIdRef: ref,
    selectedTrackedRequestId: null,
    sending: false,
    setLocalWorkflow: setWorkflow,
    setError: vi.fn(),
    selectAgent: route.selectAgent,
    snapshot: null,
    starting: false,
    stopping: false,
    onStartClient: async () => {},
  });
  return {
    agent: current.agentDid,
    selected: current.sessionId,
    behavior: current.behaviorId,
    actions,
    route,
    sendChatMessage,
  };
}

describe("explicit session selection", () => {
  it("initializes an empty agent selection through the route owner", () => {
    const { result } = renderHook(() =>
      useHarness(initialDeployment, "old-session", null),
    );
    expect(result.current.agent).toBe("agent");
    expect(result.current.selected).toBeNull();
  });

  it("preserves agent and session selection across a temporary missing snapshot", () => {
    const { result, rerender } = renderHook(
      ({ deployment }) => useHarness(deployment, "first-setup"),
      { initialProps: { deployment: initialDeployment as DeploymentView | null } },
    );
    rerender({ deployment: null });
    expect(result.current.agent).toBe("agent");
    expect(result.current.selected).toBe("first-setup");
  });
  it("uses the principal default instead of a conflicting marked default", () => {
    const deployment = {
      ...initialDeployment,
      behaviors: initialDeployment.behaviors.map((behavior) => ({
        ...behavior,
        isDefault: behavior.behaviorId === "setup",
      })),
    };
    const { result } = renderHook(() => useHarness(deployment, "first-setup"));
    act(() => result.current.actions.startNewSession());
    expect(result.current.behavior).toBe("coding");
    expect(result.current.selected).toBeNull();
  });

  it("uses the canonical enabled fallback and clears selection when none exists", () => {
    const deployment = {
      ...initialDeployment,
      agentPrincipal: { ...initialDeployment.agentPrincipal, defaultBehaviorId: null },
      behaviors: [
        { ...initialDeployment.behaviors[0], isDefault: false, enabled: false },
        { ...initialDeployment.behaviors[1], isDefault: false },
      ],
    };
    const { result, rerender } = renderHook(({ value }) => useHarness(value, null), {
      initialProps: { value: deployment },
    });
    act(() => result.current.actions.startNewSession());
    expect(result.current.behavior).toBe("setup");
    rerender({ value: { ...deployment, behaviors: [] } });
    act(() => result.current.actions.startNewSession());
    expect(result.current.behavior).toBeNull();
  });
  it("resets session selection on explicit agent navigation, not observation", () => {
    const store = createSelectionStore({
      agentDid: "agent",
      behaviorId: "setup",
      sessionId: "first-setup",
    });
    const setSession = vi.fn();
    const route = createDesktopShellSelectionActions({
      store,
      deployments: () => [],
      setSession,
      setLocalWorkflow: vi.fn(),
      setError: vi.fn(),
    });
    route.selectAgent("agent");
    expect(store.getState().sessionId).toBe("first-setup");
    expect(setSession).not.toHaveBeenCalled();
    route.selectAgent("another-agent");
    expect(store.getState().sessionId).toBeNull();
    expect(setSession).toHaveBeenCalledWith(null);
  });

  it("keeps a fresh composer after choosing Setup and creates a new session on send", async () => {
    const { result, rerender } = renderHook(
      ({ deployment }) => useHarness(deployment, "first-setup"),
      { initialProps: { deployment: initialDeployment } },
    );
    act(() => result.current.actions.startNewSession());
    expect(result.current.selected).toBeNull();
    act(() => result.current.route.selectBehavior("setup"));
    expect(result.current.behavior).toBe("setup");
    expect(result.current.selected).toBeNull();
    rerender({
      deployment: { ...initialDeployment, sessions: [...initialDeployment.sessions] },
    });
    expect(result.current.selected).toBeNull();
    await act(async () => {
      await result.current.actions.submitContent("new Setup chat");
    });
    expect(result.current.sendChatMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        sessionId: null,
        behaviorId: "setup",
        content: "new Setup chat",
      }),
    );
    expect(result.current.selected).toBe("new-setup");
    rerender({
      deployment: { ...initialDeployment, sessions: [...initialDeployment.sessions] },
    });
    expect(result.current.selected).toBe("new-setup");
  });

  it("does not select the first session when a deployment first becomes available", () => {
    const { result, rerender } = renderHook(
      ({ deployment }: { deployment: DeploymentView | null }) =>
        useHarness(deployment, null),
      { initialProps: { deployment: null as DeploymentView | null } },
    );
    rerender({ deployment: initialDeployment });
    expect(result.current.selected).toBeNull();
  });

  it("preserves an explicit session across a temporarily missing session row", () => {
    const { result, rerender } = renderHook(
      ({ deployment }) => useHarness(deployment, "first-setup"),
      { initialProps: { deployment: initialDeployment } },
    );
    rerender({ deployment: { ...initialDeployment, sessions: [] } });
    expect(result.current.selected).toBe("first-setup");
  });
});
