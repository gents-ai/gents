import { describe, expect, it, vi } from "vitest";
import { createDesktopShellChatActions } from "../src/hooks/desktopShellChatActions";

function fixture(send: () => Promise<unknown>, blocked = false) {
  const effects = {
    setLocalWorkflow: vi.fn(),
    setError: vi.fn(),
    setSending: vi.fn(),
    setOptimisticPendingTurn: vi.fn(),
    setSelectedSessionId: vi.fn(),
    setPendingMailboxCauseId: vi.fn(),
    refreshSession: vi.fn(async () => {
      throw new Error("observation unavailable");
    }),
    refreshSnapshot: vi.fn(async () => {
      throw new Error("observation unavailable");
    }),
  };
  const actions = createDesktopShellChatActions({
    ...effects,
    api: { sendChatMessage: send },
    selectedDeployment: { agentDid: "agent" },
    deployments: [],
    selectedSessionId: "session",
    pendingMailboxCauseId: null,
    behaviorReadiness: { kind: "ready", behaviorId: "coding" },
    shellProjection: {
      nonEmptyContentSendStatus: blocked
        ? {
            kind: "disabled",
            reason: "behaviorUnavailable",
            hint: "Behavior is unavailable",
          }
        : { kind: "ready" },
    },
    newSessionAgentRef: { current: null },
  } as unknown as Parameters<typeof createDesktopShellChatActions>[0]);
  return { actions, ...effects };
}

describe("canonical chat submission acceptance", () => {
  it("does not bypass a stale admission blocker when a behavior is picked and sent in one event", async () => {
    const send = vi.fn();
    const f = fixture(send, true);
    await expect(
      f.actions.submitContent("review this", "new-choice"),
    ).resolves.toBeNull();
    expect(send).not.toHaveBeenCalled();
    expect(f.setError).toHaveBeenLastCalledWith("Behavior is unavailable");
  });
  it("records an accepted pending request without depending on a successful refresh", async () => {
    const accepted = {
      sessionId: "session",
      requestId: "request",
      agentDid: "agent",
      behaviorId: "coding",
    };
    const f = fixture(async () => accepted);
    await expect(f.actions.submitContent("review this")).resolves.toEqual(accepted);
    expect(f.setOptimisticPendingTurn).toHaveBeenCalledWith(
      expect.objectContaining({
        requestId: "request",
        lifecycleState: "pending",
        content: "review this",
      }),
    );
    expect(f.setLocalWorkflow).toHaveBeenLastCalledWith({
      kind: "awaitingObservation",
      agentDid: "agent",
      sessionId: "session",
      requestId: "request",
    });
    expect(f.refreshSession).not.toHaveBeenCalled();
    expect(f.refreshSnapshot).not.toHaveBeenCalled();
    expect(f.setError).toHaveBeenLastCalledWith(null);
  });

  it("publishes a failed submission and does not invent an accepted request", async () => {
    const f = fixture(async () => {
      throw new Error("request rejected");
    });
    await expect(f.actions.submitContent("review this")).resolves.toBeNull();
    expect(f.setError).toHaveBeenLastCalledWith("Error: request rejected");
    expect(f.setLocalWorkflow).toHaveBeenLastCalledWith({ kind: "ready" });
    expect(f.setOptimisticPendingTurn).not.toHaveBeenCalled();
    expect(f.setSending).toHaveBeenLastCalledWith(false);
  });
});
