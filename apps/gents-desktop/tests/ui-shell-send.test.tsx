import { act, renderHook } from "@testing-library/react";
import { useRef, useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { createDesktopShellChatActions } from "../src/hooks/desktopShellChatActions";

const desktopShell = vi.hoisted(() => ({
  deployments: [
    {
      agentDid: "did:key:agent",
      behaviors: [],
      behaviorReadiness: {
        source: { state: "current" },
        behaviors: [{ state: "ready", behaviorId: "coding" }],
      },
    },
  ],
  behaviorReadiness: { kind: "ready", behaviorId: "coding" },
  selectedDeployment: null,
  selectedAgentDid: "did:key:agent",
  selectedSessionId: null,
  selectedTrackedRequestId: null,
  pendingMailboxCauseId: null,
  snapshot: null,
  error: null,
  sending: false,
  submitContent: vi.fn(),
  session: null,
  sessionLoad: null,
  sessionLoadingStatus: null,
  startupPhase: "ready",
  setSelectedAgentDid: vi.fn(),
  setSelectedBehaviorId: vi.fn(),
  setSelectedSessionId: vi.fn(),
  refreshSession: vi.fn(async () => null),
  refreshSnapshot: vi.fn(async () => undefined),
  onStartNewSession: vi.fn(),
  onSelectSession: vi.fn(),
  onDismissError: vi.fn(),
  onRetryStartup: vi.fn(),
  onDismissMailboxItem: vi.fn(),
  onOpenMailboxItem: vi.fn(),
  onSaveAgentConfig: vi.fn(),
  onSaveBehaviorConfig: vi.fn(),
  onDeleteBehaviorConfig: vi.fn(),
  onRemovePeer: vi.fn(),
  onRenamePeer: vi.fn(),
  retrySessionHydration: vi.fn(),
  loadOlderSessionTimeline: vi.fn(),
  onInitLocalRuntime: vi.fn(),
}));

vi.mock("../src/hooks/useDesktopShell", () => ({
  useDesktopShell: ({ api }: { api: unknown }) => {
    const [sending, setSending] = useState(false);
    const submissionInFlight = useRef(false);
    // Keep admission real: a mocked submitContent cannot prove that the adapter
    // and retry share the same synchronous owner.
    const actions = createDesktopShellChatActions({
      ...desktopShell,
      api,
      selectedDeployment: desktopShell.deployments[0],
      submissionInFlight,
      setSending,
      captureComposeIntent: () => 0,
      acceptsComposeIntent: () => true,
      setLocalWorkflow: vi.fn(),
      setOptimisticPendingTurn: vi.fn(),
      setPendingMailboxCauseId: vi.fn(),
      setError: vi.fn(),
      newSessionAgentRef: { current: null },
      shellProjection: { nonEmptyContentSendStatus: { kind: "ready" } },
      retryShellProjection: { nonEmptyContentSendStatus: { kind: "ready" } },
    } as unknown as Parameters<typeof createDesktopShellChatActions>[0]);
    desktopShell.submitContent.mockImplementation(actions.submitContent);
    return { ...desktopShell, sending, onRetryMessage: actions.onRetryMessage };
  },
}));

import { useShell } from "../src/ui/hooks/useShell";

describe("kit shell chat submission", () => {
  it("admits only one submit while the first Enter is still in flight", async () => {
    let complete!: (value: { sessionId: string; requestId: string }) => void;
    const sendChatMessage = vi.fn(
      () =>
        new Promise<{ sessionId: string; requestId: string }>((resolve) => {
          complete = resolve;
        }),
    );
    const bridge = {
      api: { sendChatMessage },
      listenToUpdates: vi.fn(),
    } as never;
    const { result } = renderHook(() => useShell(bridge, undefined));

    let first!: ReturnType<typeof result.current.sendMessage>;
    let second!: ReturnType<typeof result.current.sendMessage>;
    act(() => {
      first = result.current.sendMessage("review this", "coding");
      second = result.current.sendMessage("review this", "coding");
    });

    expect(sendChatMessage).toHaveBeenCalledOnce();
    expect(result.current.sending).toBe(true);
    await expect(second).resolves.toBeNull();

    await act(async () => {
      complete({ sessionId: "session-1", requestId: "request-1" });
      await first;
    });

    expect(result.current.sending).toBe(false);
    expect(desktopShell.submitContent).toHaveBeenCalledWith("review this", "coding");
    expect(desktopShell.setSelectedBehaviorId).not.toHaveBeenCalled();
    expect(desktopShell.refreshSession).not.toHaveBeenCalled();
    expect(desktopShell.refreshSnapshot).not.toHaveBeenCalled();
  });
});
