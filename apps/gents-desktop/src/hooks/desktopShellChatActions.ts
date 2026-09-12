import type { Dispatch, FormEvent, MutableRefObject, SetStateAction } from "react";

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

export function createDesktopShellChatActions({
  api,
  behaviorReadiness,
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
  async function submitContent(content: string): Promise<boolean> {
    const deployment = selectedDeployment ?? deployments[0] ?? null;
    if (!deployment || !content.trim()) {
      return false;
    }

    if (shellProjection.nonEmptyContentSendStatus.kind !== "ready") {
      setError(shellProjection.nonEmptyContentSendStatus.hint);
      return false;
    }
    if (behaviorReadiness.kind !== "ready") {
      return false;
    }

    setLocalWorkflow({
      kind: "submittingRequest",
      agentDid: deployment.agentDid,
      sessionId: selectedSessionId,
    });
    setSending(true);
    setError(null);
    try {
      const result = await api.sendChatMessage({
        agentDid: deployment.agentDid,
        behaviorId: behaviorReadiness.behaviorId,
        sessionId: selectedSessionId,
        content,
        causedBySourceDocId: pendingMailboxCauseId,
      });
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
      return true;
    } catch (err) {
      setLocalWorkflow({ kind: "ready" });
      setError(String(err));
      return false;
    } finally {
      setSending(false);
    }
  }

  async function onSendMessage(event: FormEvent) {
    event.preventDefault();
    if (await submitContent(draft)) {
      setDraft("");
    }
  }

  /** Retry the persisted interactive predecessor through the fenced retry API. */
  async function retryRequest(requestId: string) {
    if (!selectedDeployment) {
      return;
    }
    if (retryShellProjection.nonEmptyContentSendStatus.kind !== "ready") {
      setError(retryShellProjection.nonEmptyContentSendStatus.hint);
      return;
    }
    setLocalWorkflow({
      kind: "submittingRequest",
      agentDid: selectedDeployment.agentDid,
      sessionId: selectedSessionId,
    });
    setSending(true);
    setError(null);
    try {
      const result = await api.retryRequest(requestId);
      setSelectedSessionId(result.sessionId);
      setLocalWorkflow({
        kind: "awaitingObservation",
        agentDid: selectedDeployment.agentDid,
        sessionId: result.sessionId,
        requestId: result.requestId,
      });
    } catch (err) {
      setLocalWorkflow({ kind: "ready" });
      setError(String(err));
    } finally {
      setSending(false);
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
    setPendingMailboxCauseId(null);
    const nextBehaviorId =
      behaviorId &&
      deployment.behaviors.some((behavior) => behavior.behaviorId === behaviorId)
        ? behaviorId
        : (deployment.behaviors.find((behavior) => behavior.isDefault)?.behaviorId ??
          deployment.behaviors[0]?.behaviorId ??
          null);
    if (nextBehaviorId) {
      setSelectedBehaviorId(nextBehaviorId);
    }
    newSessionAgentRef.current = deployment.agentDid;
    setSelectedSessionId(null);
    setSession(null);
    setLocalWorkflow({ kind: "ready" });
    setError(null);
  }

  return {
    onRenameSessionTitle,
    onRetryMessage,
    onSelectSession,
    onSendMessage,
    onStartNewSession,
  };
}
