/* Adapts useDesktopShell to the prototype Shell shape so copied screens
   keep calling shell.api / applyConfig / saveBehaviorConfig. */
import { useCallback, useEffect, useMemo, useState } from "react";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";

import { useDesktopShell, type DesktopShellBridge } from "../../hooks/useDesktopShell";

export type ShellBridge = DesktopShellBridge & {
  invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
};

export function useShell(bridge: ShellBridge, routeSessionId: string | null) {
  const d = useDesktopShell(bridge);
  const api = bridge.api;

  useEffect(() => {
    if (routeSessionId === d.selectedSessionId) return;
    if (routeSessionId) d.setSelectedSessionId(routeSessionId);
  }, [routeSessionId, d.selectedSessionId, d.setSelectedSessionId]);

  const applyConfig = useCallback(
    async (run: (api: DesktopApiAdapter) => Promise<DesktopClientSnapshot>) => {
      const snapshot = await run(api);
      await d.refreshSnapshot();
      return snapshot;
    },
    [api, d],
  );

  const sendMessage = useCallback(
    async (content: string, behaviorId: string | null) => {
      if (!d.selectedAgentDid) return null;
      return api.sendChatMessage({
        agentDid: d.selectedAgentDid,
        behaviorId,
        sessionId: d.selectedSessionId,
        content,
        causedBySourceDocId: d.pendingMailboxCauseId ?? null,
      });
    },
    [api, d],
  );

  const [behaviorColors] = useState<Record<string, number>>({});

  return useMemo(() => {
    const deployments = d.deployments;
    const selectedDeployment = d.selectedDeployment;
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
      sending: d.sending,
      deployments,
      selectedDeployment,
      selectedAgentDid: d.selectedAgentDid,
      selectAgent: d.setSelectedAgentDid,
      selectedSessionId: d.selectedSessionId,
      selectedSession: d.session,
      selectedTrackedRequestId: d.activeRequestId ?? null,
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
      startupPhase: d.startupPhase,
    };
  }, [api, applyConfig, behaviorColors, d, sendMessage]);
}

export type Shell = ReturnType<typeof useShell>;
