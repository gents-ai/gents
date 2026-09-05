import { useCallback, useEffect, useMemo, useState, type SetStateAction } from "react";

import {
  projectChatShell,
  reconcileProjectedWorkflow,
  type ChatWorkflowState,
  type OptimisticPendingTurn,
} from "@source-inc/gents-desktop-chat";
import type {
  ConversationSummary,
  DeploymentView,
  DesktopSessionSnapshot,
  SyncHealthView,
} from "@source-inc/gents-desktop-client";
import {
  isTerminalTurnState,
  projectDeploymentOperationalState,
  selectedBehaviorReadinessDecision,
} from "@source-inc/gents-desktop-client";
import { trackedRequestIdForSession } from "./desktopShellRuntime";

type ChatProjectionStateOptions = {
  clientAvailable: boolean;
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  selectedConversation: ConversationSummary | null;
  selectedDeployment: DeploymentView | null;
  selectedSessionId: string | null;
  sending: boolean;
  session: DesktopSessionSnapshot | null;
  syncHealth: SyncHealthView | null;
};

/** Own local compose state and reconcile it with the bounded durable projection. */
export function useDesktopChatProjectionState({
  clientAvailable,
  selectedAgentDid,
  selectedBehaviorId,
  selectedConversation,
  selectedDeployment,
  selectedSessionId,
  sending,
  session,
  syncHealth,
}: ChatProjectionStateOptions) {
  const [localWorkflow, setLocalWorkflow] = useState<ChatWorkflowState>({
    kind: "ready",
  });
  const [optimisticPendingTurn, setOptimisticPendingTurn] =
    useState<OptimisticPendingTurn | null>(null);
  const [draftsByContext, setDraftsByContext] = useState<Record<string, string>>({});
  const operationalState = useMemo(
    () =>
      selectedDeployment
        ? projectDeploymentOperationalState(
            selectedDeployment,
            selectedBehaviorId,
            syncHealth,
          )
        : null,
    [selectedBehaviorId, selectedDeployment, syncHealth],
  );
  const behaviorReadiness =
    operationalState?.behaviorReadiness ??
    selectedBehaviorReadinessDecision(null, selectedBehaviorId);
  const retryOperationalState = useMemo(
    () =>
      selectedDeployment
        ? projectDeploymentOperationalState(
            selectedDeployment,
            session?.behaviorId ?? null,
            syncHealth,
          )
        : null,
    [selectedDeployment, session?.behaviorId, syncHealth],
  );
  const draftContextKey = JSON.stringify(
    selectedSessionId
      ? ["session", selectedAgentDid, selectedSessionId]
      : ["new", selectedAgentDid, behaviorReadiness.behaviorId],
  );
  const draft = draftsByContext[draftContextKey] ?? "";
  const setDraft = useCallback(
    (next: SetStateAction<string>) => {
      setDraftsByContext((current) => {
        const currentDraft = current[draftContextKey] ?? "";
        const nextDraft = typeof next === "function" ? next(currentDraft) : next;
        if (nextDraft === currentDraft) return current;
        if (!nextDraft) {
          const remaining = { ...current };
          delete remaining[draftContextKey];
          return remaining;
        }
        return { ...current, [draftContextKey]: nextDraft };
      });
    },
    [draftContextKey],
  );
  const shellProjection = useMemo(() => {
    return projectChatShell({
      clientAvailable,
      selectedAgentDid,
      selectedSessionId,
      draft,
      sending,
      session,
      selectedConversation,
      localWorkflow,
      operationalState,
    });
  }, [
    clientAvailable,
    draft,
    localWorkflow,
    selectedAgentDid,
    selectedConversation,
    operationalState,
    selectedSessionId,
    sending,
    session,
  ]);
  const retryShellProjection = useMemo(() => {
    return projectChatShell({
      clientAvailable,
      selectedAgentDid,
      selectedSessionId,
      draft: "",
      sending,
      session,
      selectedConversation,
      localWorkflow,
      operationalState: retryOperationalState,
    });
  }, [
    clientAvailable,
    localWorkflow,
    retryOperationalState,
    selectedAgentDid,
    selectedConversation,
    selectedDeployment,
    selectedSessionId,
    sending,
    session,
  ]);

  useEffect(() => {
    setLocalWorkflow((current) =>
      reconcileProjectedWorkflow(current, shellProjection.workflow),
    );
  }, [shellProjection.workflow]);

  useEffect(() => {
    setOptimisticPendingTurn((current) => {
      if (!current || current.sessionId !== session?.sessionId) return current;
      const durableOwner = session.timelineItems.some(
        (item) =>
          (item.kind === "pendingUserTurn" && item.requestId === current.requestId) ||
          (item.kind === "userMessage" && item.requestId === current.requestId),
      );
      return durableOwner ? null : current;
    });
  }, [session]);

  const selectedTrackedRequestId =
    trackedRequestIdForSession(selectedSessionId, shellProjection.workflow) ??
    (!isTerminalTurnState(shellProjection.turnState)
      ? shellProjection.activeRequestId
      : null);

  return {
    draft,
    setDraft,
    localWorkflow,
    setLocalWorkflow,
    optimisticPendingTurn,
    setOptimisticPendingTurn,
    operationalState,
    behaviorReadiness,
    shellProjection,
    retryShellProjection,
    selectedTrackedRequestId,
  };
}
