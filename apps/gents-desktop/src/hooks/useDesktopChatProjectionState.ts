import { useCallback, useEffect, useMemo, useState, type SetStateAction } from "react";

import {
  projectChatShell,
  reconcileProjectedWorkflow,
  type ChatWorkflowState,
  type OptimisticPendingTurn,
} from "@source-inc/gents-desktop-chat";
import type {
  SessionSummary,
  DeploymentView,
  SyncHealthView,
} from "@source-inc/gents-desktop-client";
import {
  isTerminalTurnState,
  projectDeploymentOperationalState,
  selectedBehaviorReadinessDecision,
} from "@source-inc/gents-desktop-client";
import { trackedRequestIdForSession } from "./desktopShellRuntime";
import { createDraftStore, readDraft, writeDraft } from "./draftStore";
import type { SessionHeader } from "./sessionStore";

type ChatProjectionStateOptions = {
  clientAvailable: boolean;
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  selectedSessionSummary: SessionSummary | null;
  selectedDeployment: DeploymentView | null;
  selectedSessionId: string | null;
  sending: boolean;
  session: SessionHeader | null;
  /** the requests whose user row the transcript holds */
  userRequestIds: ReadonlySet<string>;
  syncHealth: SyncHealthView | null;
};

/** Own local compose state and reconcile it with the bounded durable projection. */
export function useDesktopChatProjectionState({
  clientAvailable,
  selectedAgentDid,
  selectedBehaviorId,
  selectedSessionSummary,
  selectedDeployment,
  selectedSessionId,
  sending,
  session,
  userRequestIds,
  syncHealth,
}: ChatProjectionStateOptions) {
  const [localWorkflow, setLocalWorkflow] = useState<ChatWorkflowState>({
    kind: "ready",
  });
  const [optimisticPendingTurn, setOptimisticPendingTurn] =
    useState<OptimisticPendingTurn | null>(null);
  const [draftStore] = useState(createDraftStore);
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
  /* the draft itself is read by the composer, not here: the workflow does
     not depend on it, and a keystroke must not re-render the shell */
  const setDraft = useCallback(
    (next: SetStateAction<string>) => writeDraft(draftStore, draftContextKey, next),
    [draftStore, draftContextKey],
  );
  const readCurrentDraft = useCallback(
    () => readDraft(draftStore, draftContextKey),
    [draftStore, draftContextKey],
  );
  const shellProjection = useMemo(() => {
    return projectChatShell({
      clientAvailable,
      selectedAgentDid,
      selectedSessionId,
      draft: "",
      sending,
      session,
      selectedSessionSummary,
      localWorkflow,
      operationalState,
    });
  }, [
    clientAvailable,
    localWorkflow,
    selectedAgentDid,
    selectedSessionSummary,
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
      selectedSessionSummary,
      localWorkflow,
      operationalState: retryOperationalState,
    });
  }, [
    clientAvailable,
    localWorkflow,
    retryOperationalState,
    selectedAgentDid,
    selectedSessionSummary,
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

  /* the optimistic turn stands in for a sent message until the transcript
     holds its durable row; derived, so it ends whichever arrives first */
  const visiblePendingTurn =
    optimisticPendingTurn &&
    !(
      optimisticPendingTurn.sessionId === session?.sessionId &&
      userRequestIds.has(optimisticPendingTurn.requestId)
    )
      ? optimisticPendingTurn
      : null;

  const selectedTrackedRequestId =
    trackedRequestIdForSession(selectedSessionId, shellProjection.workflow) ??
    (!isTerminalTurnState(shellProjection.turnState)
      ? shellProjection.activeRequestId
      : null);

  return {
    draftStore,
    draftContextKey,
    readCurrentDraft,
    setDraft,
    localWorkflow,
    setLocalWorkflow,
    optimisticPendingTurn: visiblePendingTurn,
    setOptimisticPendingTurn,
    operationalState,
    behaviorReadiness,
    shellProjection,
    retryShellProjection,
    selectedTrackedRequestId,
  };
}
