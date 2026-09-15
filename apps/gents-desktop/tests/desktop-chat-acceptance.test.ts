import { describe, expect, it, vi } from "vitest";
import {
  projectChatShell,
  type ChatWorkflowState,
} from "@source-inc/gents-desktop-chat";
import { projectDeploymentOperationalState } from "@source-inc/gents-desktop-client";
import {
  createDesktopShellChatActions,
  releaseOwnedSubmissionWorkflow,
} from "../src/hooks/desktopShellChatActions";

function fixture(
  send: () => Promise<unknown>,
  blocked = false,
  retry: (requestId: string) => Promise<unknown> = async () => null,
) {
  let intentGeneration = 0;
  let workflow: ChatWorkflowState = { kind: "ready" };
  const effects = {
    setLocalWorkflow: vi.fn(
      (
        next: ChatWorkflowState | ((current: ChatWorkflowState) => ChatWorkflowState),
      ) => {
        workflow = typeof next === "function" ? next(workflow) : next;
      },
    ),
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
    acceptsComposeIntent: (captured: number) => captured === intentGeneration,
    advanceComposeIntent: () => {
      intentGeneration += 1;
    },
    captureComposeIntent: () => intentGeneration,
    api: { sendChatMessage: send, retryRequest: retry },
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
    retryShellProjection: {
      nonEmptyContentSendStatus: { kind: "ready" },
    },
    newSessionAgentRef: { current: null },
  } as unknown as Parameters<typeof createDesktopShellChatActions>[0]);
  return {
    actions,
    advanceComposeIntent: () => {
      intentGeneration += 1;
    },
    getWorkflow: () => workflow,
    ...effects,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("canonical chat submission acceptance", () => {
  it("releases only the submitting workflow owned by the completed callback", () => {
    const owned: ChatWorkflowState = {
      kind: "submittingRequest",
      agentDid: "agent-a",
      sessionId: "session-a",
    };
    const newer: ChatWorkflowState = {
      kind: "submittingRequest",
      agentDid: "agent-b",
      sessionId: "session-b",
    };
    expect(releaseOwnedSubmissionWorkflow(owned, owned)).toEqual({ kind: "ready" });
    expect(releaseOwnedSubmissionWorkflow(newer, owned)).toBe(newer);

    const deployment = {
      agentDid: "agent-b",
      source: "enrollment",
      dialSucceeded: true,
      chatSafe: true,
      lastError: null,
      runtime: null,
      agentPrincipal: { agentDid: "agent-b", defaultBehaviorId: "coding" },
      behaviorReadiness: {
        source: { state: "current" },
        activeGeneration: 1,
        routerGeneration: 1,
        updatedAt: "2026-09-15T00:00:00Z",
        behaviors: [{ state: "ready", behaviorId: "coding" }],
      },
      behaviors: [
        {
          behaviorId: "coding",
          displayName: "Coding",
          enabled: true,
          isDefault: true,
        },
      ],
    };
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "agent-b",
      selectedSessionId: null,
      draft: "follow up",
      sending: false,
      session: null,
      selectedSessionSummary: null,
      localWorkflow: releaseOwnedSubmissionWorkflow(owned, owned),
      operationalState: projectDeploymentOperationalState(
        deployment as Parameters<typeof projectDeploymentOperationalState>[0],
        "coding",
      ),
    });
    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.nonEmptyContentSendStatus).toEqual({ kind: "ready" });
  });
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
    expect(f.getWorkflow()).toEqual({
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
    expect(f.getWorkflow()).toEqual({ kind: "ready" });
    expect(f.setOptimisticPendingTurn).not.toHaveBeenCalled();
    expect(f.setSending).toHaveBeenLastCalledWith(false);
  });

  it("keeps a durable acceptance but ignores its stale presentation effects", async () => {
    const pending = deferred<{
      sessionId: string;
      requestId: string;
      agentDid: string;
      behaviorId: string;
    }>();
    const f = fixture(() => pending.promise);
    const submitted = f.actions.submitContent("review this");
    f.advanceComposeIntent();
    f.advanceComposeIntent();
    vi.clearAllMocks();
    const accepted = {
      sessionId: "origin-session",
      requestId: "request",
      agentDid: "agent",
      behaviorId: "coding",
    };
    pending.resolve(accepted);

    await expect(submitted).resolves.toEqual(accepted);
    expect(f.setSelectedSessionId).not.toHaveBeenCalled();
    expect(f.setOptimisticPendingTurn).not.toHaveBeenCalled();
    expect(f.getWorkflow()).toEqual({ kind: "ready" });
    expect(f.setPendingMailboxCauseId).not.toHaveBeenCalled();
    expect(f.setError).not.toHaveBeenCalled();
    expect(f.setSending).toHaveBeenLastCalledWith(false);
  });

  it("does not publish a stale submission failure into the new compose intent", async () => {
    const pending = deferred<never>();
    const f = fixture(() => pending.promise);
    const submitted = f.actions.submitContent("review this");
    f.advanceComposeIntent();
    vi.clearAllMocks();
    pending.reject(new Error("old route failed"));

    await expect(submitted).resolves.toBeNull();
    expect(f.getWorkflow()).toEqual({ kind: "ready" });
    expect(f.setError).not.toHaveBeenCalled();
    expect(f.setSending).toHaveBeenLastCalledWith(false);
  });

  it("ignores a stale retry acknowledgment after selection changes", async () => {
    const pending = deferred<{
      sessionId: string;
      requestId: string;
    }>();
    const f = fixture(
      async () => null,
      false,
      () => pending.promise,
    );
    const retried = f.actions.onRetryMessage("failed-request");
    f.advanceComposeIntent();
    vi.clearAllMocks();
    pending.resolve({ sessionId: "origin-session", requestId: "retry-request" });

    await retried;
    expect(f.setSelectedSessionId).not.toHaveBeenCalled();
    expect(f.getWorkflow()).toEqual({ kind: "ready" });
    expect(f.setError).not.toHaveBeenCalled();
    expect(f.setSending).toHaveBeenLastCalledWith(false);
  });

  it("does not publish a stale retry failure into the new selection", async () => {
    const pending = deferred<never>();
    const f = fixture(
      async () => null,
      false,
      () => pending.promise,
    );
    const retried = f.actions.onRetryMessage("failed-request");
    f.advanceComposeIntent();
    vi.clearAllMocks();
    pending.reject(new Error("old retry failed"));

    await retried;
    expect(f.setSelectedSessionId).not.toHaveBeenCalled();
    expect(f.getWorkflow()).toEqual({ kind: "ready" });
    expect(f.setError).not.toHaveBeenCalled();
    expect(f.setSending).toHaveBeenLastCalledWith(false);
  });
});
