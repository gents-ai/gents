/* Adapts useDesktopShell to the prototype Shell shape so copied screens
   keep calling shell.api / applyConfig / saveBehaviorConfig. */
import { useCallback, useEffect, useMemo, useRef } from "react";

import { useDesktopShell, type DesktopShellBridge } from "../../hooks/useDesktopShell";
import { toast } from "sonner";

export type ShellBridge = DesktopShellBridge & {
  invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
};

export function useShell(
  bridge: ShellBridge,
  routeSessionId: string | null | undefined,
) {
  /* a failed action is over by the time it is reported: one toast where the
     person is. The banner is for the client's own state. */
  const d = useDesktopShell({ reportFailure: toast, ...bridge });
  const api = bridge.api;
  const shellRef = useRef(d);
  shellRef.current = d;

  /* the route says which session to show; one owner follows it */
  useEffect(() => {
    if (routeSessionId !== undefined) shellRef.current.followRoute(routeSessionId);
  }, [routeSessionId]);
  useEffect(() => {
    if (routeSessionId) shellRef.current.followRoute(routeSessionId, true);
  }, [routeSessionId, d.deployments]);

  const sendMessage = useCallback(
    async (content: string, behaviorId: string | null) => {
      return d.sendMessage(content, behaviorId);
    },
    [api, d],
  );

  return useMemo(() => {
    const deployments = d.deployments;
    const selectedDeployment = d.selectedDeployment ?? deployments[0] ?? null;
    const behaviorDescriptions = Object.fromEntries(
      (selectedDeployment?.behaviors ?? []).map((b) => [
        b.behaviorId,
        b.description ?? "",
      ]),
    );
    return {
      /* the app itself, for the provider at the root */
      app: d.app,
      api,
      snapshot: d.snapshot,
      error: d.error,
      activityStatus: d.activityStatus,
      nonEmptyContentSendStatus: d.nonEmptyContentSendStatus,
      interruptVisible: d.interruptVisible,
      activeRequestId: d.activeRequestId,
      sending: d.sending,
      deployments,
      selectedDeployment,
      selectedAgentDid: d.selectedAgentDid ?? selectedDeployment?.agentDid ?? null,
      selectAgent: d.selectAgent,
      selectSession: d.selectSession,
      selectBehavior: d.selectBehavior,
      selectedBehaviorId: d.behaviorReadiness.behaviorId,
      selectedSessionId: d.selectedSessionId,
      /* the selected session lives in this store; screens select what they
         draw through useSelectedSession, so a chunk reaches only its readers */
      sessionStore: d.sessionStore,
      /* nodes, sessions and lineage by key, read through useFleet */
      fleet: d.fleet,
      /* the composer reads its draft through useDraft(draftStore, draftKey) */
      draftStore: d.draftStore,
      draftKey: d.draftContextKey,
      chatFolder: d.chatFolder,
      setChatFolder: d.setChatFolder,
      // `activeRequestId` is the newest durable request even after it has
      // completed. The tracking cursor is the one that retires at terminality
      // and therefore owns the composer/interrupt state.
      selectedTrackedRequestId: d.selectedTrackedRequestId ?? null,
      sessionLoad: d.sessionLoad,
      conversationLoading: d.sessionLoadingStatus,
      clearError: d.onDismissError,
      retryStartup: d.retryStartup,
      sendMessage,
      captureComposeIntent: d.captureComposeIntent,
      acceptsComposeIntent: d.acceptsComposeIntent,
      retryMessage: d.retryMessage,
      runTask: d.runTask,
      runSchedule: d.runSchedule,
      dismissMailboxItem: d.dismissMailboxItem,
      openMailboxItem: d.openMailboxItem,
      answerMailboxQuestion: d.answerMailboxQuestion,
      // Putting the armed reply down makes the next message an ordinary one
      // and leaves the item open.
      clearMailboxCause: d.clearMailboxCause,
      mailboxCause: d.pendingMailboxCauseId
        ? {
            itemId: d.pendingMailboxCauseId,
            behaviorId: d.selectedBehaviorId ?? "",
            sessionId: d.selectedSessionId,
          }
        : null,
      behaviorDescriptions,
      removePeer: d.removePeer,
      renamePeer: d.renamePeer,
      retrySessionHydration: d.retrySessionHydration,
      loadOlderSessionTimeline: d.loadOlderSessionTimeline,
      refreshSnapshot: d.refreshSnapshot,
      refreshSession: d.refreshSession,
      initLocalRuntime: d.initLocalRuntime,
      startupPhase: d.startupPhase,
      incompatibleHome: d.incompatibleHome,
      managedServerWait: d.managedServerWait,
      diagnosticsHint: d.diagnosticsHint,
      skipManagedServerWait: d.skipManagedServerWait,
      restartManagedServer: d.canRestartManagedServer
        ? d.restartManagedServer
        : undefined,
    };
  }, [api, d, sendMessage]);
}

export type Shell = ReturnType<typeof useShell>;
