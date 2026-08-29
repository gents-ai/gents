import { useCallback, useEffect, useMemo, useState, type SetStateAction } from "react";

import {
  isTerminalTurnState,
  projectChatShell,
  reconcileProjectedWorkflow,
  type ChatWorkflowState,
  type OptimisticPendingTurn,
} from "@source-inc/gents-desktop-chat";
import type {
  ConversationSummary,
  DeploymentView,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { selectedBehaviorReadinessDecision } from "../lib/behaviorReadiness";
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
}: ChatProjectionStateOptions) {
  const [localWorkflow, setLocalWorkflow] = useState<ChatWorkflowState>({
    kind: "ready",
  });
  const [optimisticPendingTurn, setOptimisticPendingTurn] =
    useState<OptimisticPendingTurn | null>(null);
  const [draftsByContext, setDraftsByContext] = useState<Record<string, string>>({});
  const behaviorReadiness = useMemo(
    () => selectedBehaviorReadinessDecision(selectedDeployment, selectedBehaviorId),
    [selectedBehaviorId, selectedDeployment],
  );
  const retryBehaviorReadiness = useMemo(
    () =>
      selectedBehaviorReadinessDecision(
        selectedDeployment,
        session?.behaviorId ?? null,
      ),
    [selectedDeployment, session?.behaviorId],
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
      chatSafe: selectedDeployment?.chatSafe ?? false,
      behaviorReadiness,
    });
  }, [
    behaviorReadiness,
    clientAvailable,
    draft,
    localWorkflow,
    selectedAgentDid,
    selectedConversation,
    selectedDeployment,
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
      chatSafe: selectedDeployment?.chatSafe ?? false,
      behaviorReadiness: retryBehaviorReadiness,
    });
  }, [
    clientAvailable,
    localWorkflow,
    retryBehaviorReadiness,
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
    behaviorReadiness,
    shellProjection,
    retryShellProjection,
    selectedTrackedRequestId,
  };
}
