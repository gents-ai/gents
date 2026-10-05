/* Adapts useDesktopShell to the prototype Shell shape so copied screens
   keep calling shell.api / applyConfig / saveBehaviorConfig. */
import { useCallback, useEffect, useMemo, useRef } from "react";

import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

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

  const configApi = useMemo(
    () => ({
      applyConfigComponents: d.onApplyConfigComponents,
      deleteBackendConfig: d.onDeleteBackendConfig,
      deleteBehaviorConfig: d.onDeleteBehaviorConfig,
      deleteContextConfig: d.onDeleteContextConfig,
      deleteEventSourceConfig: d.onDeleteEventSourceConfig,
      deleteInferenceProfileConfig: d.onDeleteInferenceProfileConfig,
      deleteScheduleConfig: d.onDeleteScheduleConfig,
      deleteSkillConfig: d.onDeleteSkillConfig,
      deleteTaskConfig: d.onDeleteTaskConfig,
      deleteToolsConfig: d.onDeleteToolsConfig,
      deleteToolServiceConfig: d.onDeleteToolServiceConfig,
      deleteTriggerConfig: d.onDeleteTriggerConfig,
      patchConfigComponents: d.onPatchConfigComponents,
      saveAgentConfig: d.onSaveAgentConfig,
      saveBackendConfig: d.onSaveBackendConfig,
      saveBehaviorConfig: d.onSaveBehaviorConfig,
      setDefaultBehavior: d.onSetDefaultBehavior,
      saveEventSourceConfig: d.onSaveEventSourceConfig,
      saveInferenceProfileConfig: d.onSaveInferenceProfileConfig,
      saveScheduleConfig: d.onSaveScheduleConfig,
      saveSkillConfig: d.onSaveSkillConfig,
      saveTaskConfig: d.onSaveTaskConfig,
      saveToolsConfig: d.onSaveToolsConfig,
      saveToolServiceConfig: d.onSaveToolServiceConfig,
      saveTriggerConfig: d.onSaveTriggerConfig,
    }),
    [d],
  );

  const applyConfig = useCallback(
    (run: (api: typeof configApi) => Promise<DesktopClientSnapshot>) => run(configApi),
    [configApi],
  );

  const sendMessage = useCallback(
    async (content: string, behaviorId: string | null) => {
      return d.submitContent(content, behaviorId);
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
      retryStartup: d.onRetryStartup,
      sendMessage,
      captureComposeIntent: d.captureComposeIntent,
      acceptsComposeIntent: d.acceptsComposeIntent,
      retryMessage: d.onRetryMessage,
      runTask: d.onRunTask,
      runSchedule: d.onRunSchedule,
      dismissMailboxItem: d.onDismissMailboxItem,
      openMailboxItem: d.onOpenMailboxItem,
      answerMailboxQuestion: d.onAnswerMailboxQuestion,
      // Putting the armed reply down makes the next message an ordinary one
      // and leaves the item open.
      clearMailboxCause: d.clearPendingMailboxCause,
      mailboxCause: d.pendingMailboxCauseId
        ? {
            itemId: d.pendingMailboxCauseId,
            behaviorId: d.selectedBehaviorId ?? "",
            sessionId: d.selectedSessionId,
          }
        : null,
      saveAgentConfig: d.onSaveAgentConfig,
      saveBehaviorConfig: d.onSaveBehaviorConfig,
      deleteBehaviorConfig: d.onDeleteBehaviorConfig,
      behaviorDescriptions,
      removePeer: d.onRemovePeer,
      renamePeer: d.onRenamePeer,
      applyConfig,
      retrySessionHydration: d.retrySessionHydration,
      loadOlderSessionTimeline: d.loadOlderSessionTimeline,
      refreshSnapshot: d.refreshSnapshot,
      refreshSession: d.refreshSession,
      onInitLocalRuntime: d.onInitLocalRuntime,
      startupPhase: d.startupPhase,
      incompatibleHome: d.incompatibleHome,
      managedServerWait: d.managedServerWait,
      diagnosticsHint: d.diagnosticsHint,
      skipManagedServerWait: d.onSkipManagedServerWait,
      restartManagedServer: d.canRestartManagedServer
        ? d.onRestartManagedServer
        : undefined,
    };
  }, [api, applyConfig, d, sendMessage]);
}

export type Shell = ReturnType<typeof useShell>;
