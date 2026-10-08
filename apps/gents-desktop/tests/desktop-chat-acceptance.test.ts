import { describe, expect, it, vi } from "vitest";
import {
  projectChatShell,
  type ChatWorkflowState,
} from "@source-inc/gents-desktop-chat";
import { projectDeploymentOperationalState } from "@source-inc/gents-desktop-client";
import { releaseOwnedSubmissionWorkflow } from "../src/hooks/chatStore";
import { createDesktopShellChatActions } from "../src/hooks/desktopShellChatActions";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { selection } from "../src/hooks/selectionStore";
import { admittingProjection, historyOf, shellStores } from "./shell-fixture";

function fixture(
  send: () => Promise<unknown>,
  blocked = false,
  retry: (requestId: string) => Promise<unknown> = async () => null,
) {
  const stores = shellStores({
    deployments: [{ agentDid: "agent", sessions: [], mailboxItems: [] }],
    selection: { agentDid: "agent", sessionId: "session" },
  });
  const store = stores.selection;
  const effects = {
    reportFailure: vi.fn(),
    refreshSession: vi.fn(async () => {
      throw new Error("observation unavailable");
    }),
    refreshSnapshot: vi.fn(async () => {
      throw new Error("observation unavailable");
    }),
  };
  const sending = historyOf(stores, "sending");
  const actions = createDesktopShellChatActions({
    ...effects,
    api: { sendChatMessage: send, retryRequest: retry } as unknown as DesktopApiAdapter,
    stores,
    project: () =>
      admittingProjection("coding", blocked ? "Behavior is unavailable" : undefined),
  });
  return {
    actions,
    store,
    sending,
    advanceComposeIntent: () => selection.advanceIntent(store),
    getWorkflow: () => stores.chat.getState().localWorkflow,
    pendingTurn: () => stores.chat.getState().optimisticPendingTurn,
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
  it.each(["send", "retry"])(
    "admits only one mutation before React renders (%s first)",
    async (first) => {
      const pending = deferred<unknown>();
      const send = vi.fn(() => pending.promise);
      const retry = vi.fn(() => pending.promise);
      const f = fixture(send, false, retry);
      const active =
        first === "send"
          ? f.actions.sendMessage("first")
          : f.actions.retryMessage("predecessor");
      const duplicateSend = f.actions.sendMessage("duplicate");
      const duplicateRetry = f.actions.retryMessage("predecessor");
      expect(send.mock.calls.length + retry.mock.calls.length).toBe(1);
      pending.resolve({ sessionId: "session", requestId: "accepted" });
      await Promise.all([active, duplicateSend, duplicateRetry]);
      expect(f.sending).toEqual([true, false]);
    },
  );
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
      f.actions.sendMessage("review this", "new-choice"),
    ).resolves.toBeNull();
    expect(send).not.toHaveBeenCalled();
    expect(f.reportFailure).toHaveBeenLastCalledWith("Behavior is unavailable");
  });
  it("records an accepted pending request without depending on a successful refresh", async () => {
    const accepted = {
      sessionId: "session",
      requestId: "request",
      agentDid: "agent",
      behaviorId: "coding",
    };
    const f = fixture(async () => accepted);
    await expect(f.actions.sendMessage("review this")).resolves.toEqual(accepted);
    expect(f.pendingTurn()).toEqual(
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
    expect(f.reportFailure).not.toHaveBeenCalled();
  });

  it("publishes a failed submission and does not invent an accepted request", async () => {
    const f = fixture(async () => {
      throw new Error("request rejected");
    });
    await expect(f.actions.sendMessage("review this")).resolves.toBeNull();
    expect(f.reportFailure).toHaveBeenLastCalledWith(
      "Couldn’t send the message: request rejected",
    );
    expect(f.getWorkflow()).toEqual({ kind: "ready" });
    expect(f.pendingTurn()).toBeNull();
    expect(f.sending).toEqual([true, false]);
  });

  it("keeps a durable acceptance but ignores its stale presentation effects", async () => {
    const pending = deferred<{
      sessionId: string;
      requestId: string;
      agentDid: string;
      behaviorId: string;
    }>();
    const f = fixture(() => pending.promise);
    const submitted = f.actions.sendMessage("review this");
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
    expect(f.store.getState().sessionId).toBe("session");
    expect(f.pendingTurn()).toBeNull();
    expect(f.getWorkflow()).toEqual({ kind: "ready" });
    expect(f.store.getState().mailboxRoute).toBeNull();
    expect(f.reportFailure).not.toHaveBeenCalled();
    expect(f.sending).toEqual([true, false]);
  });

  it("does not publish a stale submission failure into the new compose intent", async () => {
    const pending = deferred<never>();
    const f = fixture(() => pending.promise);
    const submitted = f.actions.sendMessage("review this");
    f.advanceComposeIntent();
    vi.clearAllMocks();
    pending.reject(new Error("old route failed"));

    await expect(submitted).resolves.toBeNull();
    expect(f.getWorkflow()).toEqual({ kind: "ready" });
    expect(f.reportFailure).not.toHaveBeenCalled();
    expect(f.sending).toEqual([true, false]);
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
    const retried = f.actions.retryMessage("failed-request");
    f.advanceComposeIntent();
    vi.clearAllMocks();
    pending.resolve({ sessionId: "origin-session", requestId: "retry-request" });

    await retried;
    expect(f.store.getState().sessionId).toBe("session");
    expect(f.getWorkflow()).toEqual({ kind: "ready" });
    expect(f.reportFailure).not.toHaveBeenCalled();
    expect(f.sending).toEqual([true, false]);
  });

  it("does not publish a stale retry failure into the new selection", async () => {
    const pending = deferred<never>();
    const f = fixture(
      async () => null,
      false,
      () => pending.promise,
    );
    const retried = f.actions.retryMessage("failed-request");
    f.advanceComposeIntent();
    vi.clearAllMocks();
    pending.reject(new Error("old retry failed"));

    await retried;
    expect(f.store.getState().sessionId).toBe("session");
    expect(f.getWorkflow()).toEqual({ kind: "ready" });
    expect(f.reportFailure).not.toHaveBeenCalled();
    expect(f.sending).toEqual([true, false]);
  });
});
