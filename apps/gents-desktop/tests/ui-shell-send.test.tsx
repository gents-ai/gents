import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const desktopShell = vi.hoisted(() => ({
  deployments: [{ agentDid: "did:key:agent", behaviors: [] }],
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
  useDesktopShell: () => desktopShell,
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
    desktopShell.submitContent.mockImplementation(sendChatMessage);
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
    expect(desktopShell.refreshSession).not.toHaveBeenCalled();
    expect(desktopShell.refreshSnapshot).not.toHaveBeenCalled();
  });
});
