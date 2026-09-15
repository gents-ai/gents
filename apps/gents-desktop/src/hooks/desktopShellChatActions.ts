import type { Dispatch, FormEvent, MutableRefObject, SetStateAction } from "react";
import {
  selectedBehaviorReadinessDecision,
  selectedBehaviorIdForDeployment,
  type ChatSendResult,
} from "@source-inc/gents-desktop-client";

import type {
  ChatShellProjection,
  ChatWorkflowState,
  OptimisticPendingTurn,
} from "@source-inc/gents-desktop-chat";
import type {
  BehaviorReadinessDecision,
  DeploymentView,
  DesktopApiAdapter,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

type ChatActionParams = {
  submissionInFlight: MutableRefObject<boolean>;
  acceptsComposeIntent: (capturedGeneration: number) => boolean;
  advanceComposeIntent: () => void;
  captureComposeIntent: () => number;
  api: DesktopApiAdapter;
  behaviorReadiness: BehaviorReadinessDecision;
  draft: string;
  newSessionAgentRef: MutableRefObject<string | null>;
  refreshSession: (
    nextSessionId: string | null,
  ) => Promise<DesktopSessionSnapshot | null>;
  refreshSnapshot: () => Promise<void>;
  selectedDeployment: DeploymentView | null;
  deployments: DeploymentView[];
  selectedSessionId: string | null;
  pendingMailboxCauseId: string | null;
  setDraft: Dispatch<SetStateAction<string>>;
  setError: Dispatch<SetStateAction<string | null>>;
  setLocalWorkflow: Dispatch<SetStateAction<ChatWorkflowState>>;
  setOptimisticPendingTurn: Dispatch<SetStateAction<OptimisticPendingTurn | null>>;
  setSelectedBehaviorId: Dispatch<SetStateAction<string | null>>;
  setSelectedSessionId: Dispatch<SetStateAction<string | null>>;
  setSending: Dispatch<SetStateAction<boolean>>;
  setPendingMailboxCauseId: Dispatch<SetStateAction<string | null>>;
  setSession: Dispatch<SetStateAction<DesktopSessionSnapshot | null>>;
  shellProjection: ChatShellProjection;
  retryShellProjection: ChatShellProjection;
};

export function releaseOwnedSubmissionWorkflow(
  current: ChatWorkflowState,
  owned: ChatWorkflowState,
): ChatWorkflowState {
  return current === owned ? { kind: "ready" } : current;
}

export function createDesktopShellChatActions({
  submissionInFlight,
  api,
  behaviorReadiness,
  acceptsComposeIntent,
  advanceComposeIntent,
  captureComposeIntent,
  draft,
  newSessionAgentRef,
  refreshSession,
  refreshSnapshot,
  selectedDeployment,
  deployments,
  selectedSessionId,
  pendingMailboxCauseId,
  setDraft,
  setError,
  setLocalWorkflow,
  setOptimisticPendingTurn,
  setSelectedBehaviorId,
  setSelectedSessionId,
  setSending,
  setPendingMailboxCauseId,
  setSession,
  shellProjection,
  retryShellProjection,
}: ChatActionParams) {
  async function submitContent(
    content: string,
    behaviorId?: string | null,
  ): Promise<ChatSendResult | null> {
    if (submissionInFlight.current) return null;
    const deployment = selectedDeployment ?? deployments[0] ?? null;
    if (!deployment || !content.trim()) {
      return null;
    }

    if (shellProjection.nonEmptyContentSendStatus.kind !== "ready") {
      setError(shellProjection.nonEmptyContentSendStatus.hint);
      return null;
    }
    const admission =
      behaviorId === undefined
        ? behaviorReadiness
        : selectedBehaviorReadinessDecision(deployment, behaviorId);
    if (admission.kind !== "ready") {
      setError("The selected behavior is unavailable");
      return null;
    }

    // Synchronous admission implements startSubmit before React renders sending.
    // Every send and retry entry point shares this owner.
    submissionInFlight.current = true;
    const intentGeneration = captureComposeIntent();
    const ownedWorkflow: ChatWorkflowState = {
      kind: "submittingRequest",
      agentDid: deployment.agentDid,
      sessionId: selectedSessionId,
    };
    setLocalWorkflow(ownedWorkflow);
    setSending(true);
    setError(null);
    try {
      const result = await api.sendChatMessage({
        agentDid: deployment.agentDid,
        behaviorId: admission.behaviorId,
        sessionId: selectedSessionId,
        content,
        causedBySourceDocId: pendingMailboxCauseId,
      });
      if (!acceptsComposeIntent(intentGeneration)) return result;
      setPendingMailboxCauseId(null);
      newSessionAgentRef.current = null;
      setSelectedSessionId(result.sessionId);
      setOptimisticPendingTurn({
        sessionId: result.sessionId,
        requestId: result.requestId,
        content,
        selectedSkillIds: [],
        lifecycleState: "pending",
        createdAt: new Date().toISOString(),
      });
      setLocalWorkflow({
        kind: "awaitingObservation",
        agentDid: deployment.agentDid,
        sessionId: result.sessionId,
        requestId: result.requestId,
      });
      return result;
    } catch (err) {
      if (!acceptsComposeIntent(intentGeneration)) return null;
      setLocalWorkflow({ kind: "ready" });
      setError(String(err));
      return null;
    } finally {
      setLocalWorkflow((current) =>
        releaseOwnedSubmissionWorkflow(current, ownedWorkflow),
      );
      setSending(false);
      submissionInFlight.current = false;
    }
  }

  async function onSendMessage(event: FormEvent) {
    event.preventDefault();
    const intentGeneration = captureComposeIntent();
    if ((await submitContent(draft)) && acceptsComposeIntent(intentGeneration)) {
      setDraft((current) => (current === draft ? "" : current));
    }
  }

  /** Retry the persisted interactive predecessor through the fenced retry API. */
  async function retryRequest(requestId: string) {
    if (submissionInFlight.current) return;
    if (!selectedDeployment) {
      return;
    }
    if (retryShellProjection.nonEmptyContentSendStatus.kind !== "ready") {
      setError(retryShellProjection.nonEmptyContentSendStatus.hint);
      return;
    }
    submissionInFlight.current = true;
    const intentGeneration = captureComposeIntent();
    const ownedWorkflow: ChatWorkflowState = {
      kind: "submittingRequest",
      agentDid: selectedDeployment.agentDid,
      sessionId: selectedSessionId,
    };
    setLocalWorkflow(ownedWorkflow);
    setSending(true);
    setError(null);
    try {
      const result = await api.retryRequest(requestId);
      if (!acceptsComposeIntent(intentGeneration)) return;
      setSelectedSessionId(result.sessionId);
      setLocalWorkflow({
        kind: "awaitingObservation",
        agentDid: selectedDeployment.agentDid,
        sessionId: result.sessionId,
        requestId: result.requestId,
      });
    } catch (err) {
      if (!acceptsComposeIntent(intentGeneration)) return;
      setLocalWorkflow({ kind: "ready" });
      setError(String(err));
    } finally {
      setLocalWorkflow((current) =>
        releaseOwnedSubmissionWorkflow(current, ownedWorkflow),
      );
      setSending(false);
      submissionInFlight.current = false;
    }
  }

  function onRetryMessage(requestId: string) {
    return retryRequest(requestId);
  }

  async function onRenameSessionTitle(sessionId: string, title: string) {
    if (!selectedDeployment) {
      return;
    }
    setError(null);
    try {
      await api.renameSession({
        agentDid: selectedDeployment.agentDid,
        sessionId,
        title,
      });
      await refreshSnapshot();
      await refreshSession(sessionId);
    } catch (err) {
      setError(String(err));
      throw err;
    }
  }

  function onSelectSession(sessionId: string) {
    advanceComposeIntent();
    setPendingMailboxCauseId(null);
    const sessionSummary = selectedDeployment?.sessions.find(
      (item) => item.sessionId === sessionId,
    );
    if (sessionSummary?.behaviorId) {
      setSelectedBehaviorId(sessionSummary.behaviorId);
    }
    newSessionAgentRef.current = null;
    if (sessionSummary?.sessionId !== sessionId) {
      setSession(null);
    }
    setSelectedSessionId(sessionId);
  }

  function onStartNewSession(behaviorId?: string | null) {
    const deployment = selectedDeployment ?? deployments[0] ?? null;
    if (!deployment) {
      return;
    }
    advanceComposeIntent();
    setPendingMailboxCauseId(null);
    setSelectedBehaviorId(
      selectedBehaviorIdForDeployment(deployment, behaviorId ?? null),
    );
    newSessionAgentRef.current = deployment.agentDid;
    setSelectedSessionId(null);
    setSession(null);
    setLocalWorkflow({ kind: "ready" });
    setError(null);
  }

  return {
    submitContent,
    onRenameSessionTitle,
    onRetryMessage,
    onSelectSession,
    onSendMessage,
    onStartNewSession,
  };
}
