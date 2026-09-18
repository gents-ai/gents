/* Adapts useDesktopShell to the prototype Shell shape so copied screens
   keep calling shell.api / applyConfig / saveBehaviorConfig. */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

import { useDesktopShell, type DesktopShellBridge } from "../../hooks/useDesktopShell";

export type ShellBridge = DesktopShellBridge & {
  invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
};

export function useShell(
  bridge: ShellBridge,
  routeSessionId: string | null | undefined,
) {
  const d = useDesktopShell(bridge);
  const api = bridge.api;
  const shellRef = useRef(d);
  shellRef.current = d;

  useEffect(() => {
    const shell = shellRef.current;
    if (routeSessionId === undefined) return;
    if (routeSessionId === null) {
      shell.onStartNewSession();
      return;
    }
    // Session selection is behavior-aware. The route must use the same owner
    // as every other session selection so reopening an older configurator
    // chat also restores that session's behavior before consistency checks run.
    shell.onSelectSession(routeSessionId);
  }, [routeSessionId]);

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

  const [behaviorColors] = useState<Record<string, number>>({});

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
      selectAgent: d.setSelectedAgentDid,
      selectBehavior: d.setSelectedBehaviorId,
      selectedBehaviorId: d.behaviorReadiness.behaviorId,
      selectedSessionId: d.selectedSessionId,
      selectedSession: d.session,
      draft: d.draft,
      setDraft: d.setDraft,
      // `activeRequestId` is the newest durable request even after it has
      // completed. The tracking cursor is the one that retires at terminality
      // and therefore owns the composer/interrupt state.
      selectedTrackedRequestId: d.selectedTrackedRequestId ?? null,
      sessionLoad: d.sessionLoad,
      conversationLoading: d.sessionLoadingStatus,
      clearError: d.onDismissError,
      reconnect: d.onRetryStartup,
      holds: [] as {
        sessionId?: string | null;
        toolCallId: string;
        toolName: string;
        args: string;
      }[],
      sendMessage,
      captureComposeIntent: d.captureComposeIntent,
      acceptsComposeIntent: d.acceptsComposeIntent,
      retryMessage: d.onRetryMessage,
      runTask: d.onRunTask,
      runSchedule: d.onRunSchedule,
      resolveHold: async (_id: string, _approve: boolean) => {},
      dismissMailboxItem: d.onDismissMailboxItem,
      openMailboxItem: d.onOpenMailboxItem,
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
      saveBehaviorDescription: async () => {},
      forkSession: async (_sessionId: string) => {
        throw new Error("session fork is not on the bridge");
      },
      behaviorColors,
      saveBehaviorColor: async () => {},
      removePeer: d.onRemovePeer,
      renamePeer: d.onRenamePeer,
      applyConfig,
      retrySessionHydration: d.retrySessionHydration,
      loadOlderSessionTimeline: d.loadOlderSessionTimeline,
      refreshSnapshot: d.refreshSnapshot,
      refreshSession: d.refreshSession,
      onInitLocalRuntime: d.onInitLocalRuntime,
      startupPhase: d.startupPhase,
    };
  }, [api, applyConfig, behaviorColors, d, sendMessage]);
}

export type Shell = ReturnType<typeof useShell>;
