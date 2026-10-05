import type { Dispatch, FormEvent, MutableRefObject, SetStateAction } from "react";
import {
  selectedBehaviorReadinessDecision,
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
import { actionFailure, shownFailure } from "./desktopShellRuntime";
import { selection, type SelectionStore } from "./selectionStore";

type ChatActionParams = {
  submissionInFlight: MutableRefObject<boolean>;
  /** the selection, read when an action runs */
  store: SelectionStore;
  api: DesktopApiAdapter;
  behaviorReadiness: BehaviorReadinessDecision;
  draft: string;
  refreshSession: (
    nextSessionId: string | null,
  ) => Promise<DesktopSessionSnapshot | null>;
  refreshSnapshot: () => Promise<void>;
  /** The folder the user works in for this chat, sent with every message. */
  chatFolder?: string | null;
  /** Called with the session a send created or continued, to keep its folder. */
  adoptChatFolder?: (sessionId: string) => void;
  selectedDeployment: DeploymentView | null;
  deployments: DeploymentView[];
  setDraft: Dispatch<SetStateAction<string>>;
  setError: Dispatch<SetStateAction<string | null>>;
  setLocalWorkflow: Dispatch<SetStateAction<ChatWorkflowState>>;
  setOptimisticPendingTurn: Dispatch<SetStateAction<OptimisticPendingTurn | null>>;
  setSending: Dispatch<SetStateAction<boolean>>;
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
  store,
  api,
  behaviorReadiness,
  draft,
  chatFolder = null,
  adoptChatFolder,
  refreshSession,
  refreshSnapshot,
  selectedDeployment,
  deployments,
  setDraft,
  setError,
  setLocalWorkflow,
  setOptimisticPendingTurn,
  setSending,
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
    const intentGeneration = selection.captureIntent(store);
    const { sessionId: selectedSessionId, mailboxRoute } = store.getState();
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
        causedBySourceDocId: mailboxRoute?.itemId ?? null,
        cwd: chatFolder,
      });
      adoptChatFolder?.(result.sessionId);
      if (!selection.acceptsIntent(store, intentGeneration)) return result;
      selection.adoptSession(store, result.sessionId);
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
      if (!selection.acceptsIntent(store, intentGeneration)) return null;
      setLocalWorkflow({ kind: "ready" });
      setError(actionFailure("send the message", err));
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
    const result = await submitContent(draft);
    if (result) {
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
    const intentGeneration = selection.captureIntent(store);
    const ownedWorkflow: ChatWorkflowState = {
      kind: "submittingRequest",
      agentDid: selectedDeployment.agentDid,
      sessionId: store.getState().sessionId,
    };
    setLocalWorkflow(ownedWorkflow);
    setSending(true);
    setError(null);
    try {
      const result = await api.retryRequest(requestId, selectedDeployment.agentDid);
      if (!selection.acceptsIntent(store, intentGeneration)) return;
      selection.settleSession(store, result.sessionId);
      setLocalWorkflow({
        kind: "awaitingObservation",
        agentDid: selectedDeployment.agentDid,
        sessionId: result.sessionId,
        requestId: result.requestId,
      });
    } catch (err) {
      if (!selection.acceptsIntent(store, intentGeneration)) return;
      setLocalWorkflow({ kind: "ready" });
      setError(actionFailure("retry the message", err));
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
      setError(actionFailure("rename the session", err));
      throw shownFailure(err);
    }
  }

  return {
    submitContent,
    onRenameSessionTitle,
    onRetryMessage,
    onSendMessage,
  };
}
