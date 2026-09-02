import { useEffect, useMemo, useRef, useState } from "react";

import { setDesktopShellTimingConfigForTests } from "./desktopShellRuntime";
import { createDesktopShellChatActions } from "./desktopShellChatActions";
import { createDesktopShellConfigActions } from "./desktopShellConfigActions";
import { useDesktopShellEffects } from "./desktopShellEffects";
import { useDesktopClientLifecycle } from "./useDesktopClientLifecycle";
import { useDesktopChatProjectionState } from "./useDesktopChatProjectionState";
import { useDesktopMailboxRoute } from "./useDesktopMailboxRoute";
import { useDesktopSessionProjection } from "./useDesktopSessionProjection";
import { createDesktopShellPeerActions } from "./desktopShellPeerActions";
import { createDesktopShellTaskActions } from "./desktopShellTaskActions";
import type {
  DesktopApiAdapter,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";
import { projectConversationLoadingStatus } from "../lib/loadingStatus";

export { setDesktopShellTimingConfigForTests };
export type { DesktopStartupPhase } from "./useDesktopClientLifecycle";

export type DesktopShellBridge = {
  api: DesktopApiAdapter;
  listenToUpdates: DesktopClientUpdatedListenerFactory;
  supportsManagedServer?: boolean;
};

export function useDesktopShell({
  api,
  listenToUpdates,
  supportsManagedServer = false,
}: DesktopShellBridge) {
  const selectedSessionIdRef = useRef<string | null>(null);
  const selectedAgentDidRef = useRef<string | null>(null);
  const selectedTrackedRequestIdRef = useRef<string | null>(null);
  const [sending, setSending] = useState(false);
  const [savingBehaviorConfig, setSavingBehaviorConfig] = useState(false);
  const [savingConfig, setSavingConfig] = useState(false);
  const [addingPeer, setAddingPeer] = useState(false);
  const [repairingP2P, setRepairingP2P] = useState(false);
  const [runningTask, setRunningTask] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const {
    session,
    sessionLoad,
    setSession,
    refreshSession,
    retrySessionHydration,
    refreshSessionLiveDelta,
    loadOlderSessionTimeline,
  } = useDesktopSessionProjection({
    api,
    selectedAgentDidRef,
    selectedSessionIdRef,
    selectedTrackedRequestIdRef,
    setError,
  });
  const {
    autostartAttempted,
    localServerAvailable,
    autoRestartInFlight,
    lastP2PAutoRestartAt,
    lastObservedP2PHealth,
    snapshot,
    setSnapshot,
    startupPhase,
    loading,
    starting,
    setStarting,
    stopping,
    refreshSnapshot,
    ensureDesktopClientStarted,
    onStartClient,
    onRetryStartup,
    restartDesktopClient,
  } = useDesktopClientLifecycle({
    api,
    supportsManagedServer,
    refreshSession,
    selectedSessionIdRef,
    setError,
    setSession,
  });
  const [selectedAgentDid, setSelectedAgentDid] = useState<string | null>(null);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [selectedBehaviorId, setSelectedBehaviorId] = useState<string | null>(null);
  const {
    newConversationAgentRef,
    pendingMailboxCauseId,
    setPendingMailboxCauseId,
    clearPendingMailboxCause,
    onOpenMailboxItem,
    onDismissMailboxItem,
    selectAgent,
    selectSession,
    selectBehavior,
  } = useDesktopMailboxRoute({
    api,
    refreshSnapshot,
    selectedAgentDid,
    selectedBehaviorId,
    selectedSessionId,
    setError,
    setSelectedAgentDid,
    setSelectedBehaviorId,
    setSelectedSessionId,
    setSession,
  });
  const deployments = snapshot?.client?.deployments ?? [];
  const selectedDeployment =
    deployments.find((deployment) => deployment.agentDid === selectedAgentDid) ?? null;
  const selectedConversation =
    selectedDeployment?.conversations.find(
      (conversation) => conversation.sessionId === selectedSessionId,
    ) ?? null;
  const selectedSessionSnapshot =
    session?.sessionId === selectedSessionId &&
    (!selectedAgentDid || !session.agentDid || session.agentDid === selectedAgentDid)
      ? session
      : null;
  const behaviorOptions = selectedDeployment?.behaviors ?? [];
  const runtimeHealth = snapshot?.client?.p2pHealth ?? null;
  const {
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
  } = useDesktopChatProjectionState({
    clientAvailable: Boolean(snapshot?.client),
    selectedAgentDid,
    selectedBehaviorId,
    selectedConversation,
    selectedDeployment,
    selectedSessionId,
    sending,
    session: selectedSessionSnapshot,
    syncHealth: snapshot?.client?.syncHealth ?? null,
  });
  const canSendMessage = shellProjection.sendStatus.kind === "ready";
  const conversationLoadingStatus = useMemo(
    () =>
      projectConversationLoadingStatus({
        selectedSessionId,
        selectedAgentDid,
        session: selectedSessionSnapshot,
        sessionLoad,
        operationalState,
      }),
    [
      operationalState,
      selectedAgentDid,
      selectedSessionId,
      selectedSessionSnapshot,
      sessionLoad,
    ],
  );
  selectedAgentDidRef.current = selectedAgentDid;
  selectedSessionIdRef.current = selectedSessionId;
  selectedTrackedRequestIdRef.current = selectedTrackedRequestId;

  useDesktopShellEffects({
    api,
    autoRestartInFlight,
    autostartAttempted,
    deployments,
    lastObservedP2PHealth,
    lastP2PAutoRestartAt,
    localWorkflow,
    localServerAvailable,
    listenToUpdates,
    newConversationAgentRef,
    onStartClient,
    refreshSession,
    refreshSessionLiveDelta,
    refreshSnapshot,
    restartDesktopClient,
    runtimeHealth,
    selectedAgentDid,
    selectedBehaviorId,
    selectedDeployment,
    selectedSessionId,
    selectedSessionIdRef,
    selectedTrackedRequestIdRef,
    selectedTrackedRequestId,
    sending,
    setLocalWorkflow,
    setError,
    setSelectedAgentDid,
    setSelectedBehaviorId,
    setSelectedSessionId,
    snapshot,
    starting,
    stopping,
  });

  const {
    onFetchPeerStatus,
    onRequestStatusEnrollment,
    onInitLocalRuntime,
    onRemovePeer,
    onRenamePeer,
    onRepairP2P,
  } = createDesktopShellPeerActions({
    api,
    snapshot,
    ensureDesktopClientStarted,
    setAddingPeer,
    setError,
    setRepairingP2P,
    setSelectedAgentDid,
    setSnapshot,
    setStarting,
  });
  const foregroundRepairRef = useRef(onRepairP2P);
  const foregroundRepairEnabledRef = useRef(Boolean(snapshot?.client));
  foregroundRepairRef.current = onRepairP2P;
  foregroundRepairEnabledRef.current = Boolean(snapshot?.client);

  useEffect(() => {
    function repairAfterForeground() {
      if (
        document.visibilityState === "visible" &&
        foregroundRepairEnabledRef.current
      ) {
        void foregroundRepairRef.current().catch(() => {});
      }
    }

    document.addEventListener("visibilitychange", repairAfterForeground);
    return () =>
      document.removeEventListener("visibilitychange", repairAfterForeground);
  }, []);

  const {
    onSaveAgentConfig,
    onSaveBackendConfig,
    onSaveBehaviorConfig,
    onDeleteSkillConfig,
    onDeleteTaskConfig,
    onDeleteScheduleConfig,
    onDeleteEventTriggerConfig,
    onDeleteBackendConfig,
    onDeleteInferenceProfileConfig,
    onDeleteToolSelectionConfig,
    onDeleteToolServiceConfig,
    onDeleteBehaviorConfig,
    onProbeInferenceEndpoint,
    onCodexLogin,
    onCancelCodexLogin,
    onGrokLogin,
    onCancelGrokLogin,
    onSaveInferenceProfileConfig,
    onSaveSkillConfig,
    onSaveToolSelectionConfig,
    onSaveToolServiceConfig,
    onTestToolService,
  } = createDesktopShellConfigActions({
    api,
    setError,
    setSavingBehaviorConfig,
    setSavingConfig,
    setSelectedAgentDid,
    setSelectedBehaviorId,
    setSnapshot,
  });

  const {
    onRenameConversationTitle,
    onRetryMessage,
    onSelectSession,
    onSendMessage,
    onStartNewConversation,
  } = createDesktopShellChatActions({
    api,
    behaviorReadiness,
    draft,
    newConversationAgentRef,
    refreshSession,
    refreshSnapshot,
    selectedDeployment,
    selectedSessionId,
    pendingMailboxCauseId,
    session: selectedSessionSnapshot,
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
  });

  const {
    onRunSchedule,
    onRunTask,
    onSaveEventTriggerConfig,
    onSaveScheduleConfig,
    onSaveTaskConfig,
  } = createDesktopShellTaskActions({
    api,
    refreshSession,
    refreshSnapshot,
    setError,
    setRunningTask,
    setSavingConfig,
    setSelectedSessionId,
    setSnapshot,
  });

  function onDismissError() {
    setError(null);
  }

  return {
    snapshot,
    session: selectedSessionSnapshot,
    sessionLoad,
    conversationLoadingStatus,
    optimisticPendingTurn,
    startupPhase,
    loading,
    starting,
    stopping,
    sending,
    savingBehaviorConfig,
    savingConfig,
    addingPeer,
    repairingP2P,
    runningTask,
    error,
    onDismissError,
    onRetryStartup,
    selectedAgentDid,
    selectedSessionId,
    selectedBehaviorId,
    pendingMailboxCauseId,
    draft,
    deployments,
    selectedDeployment,
    selectedConversation,
    behaviorOptions,
    runtimeHealth,
    operationalState,
    behaviorReadiness,
    canSendMessage,
    chatWorkflow: shellProjection.workflow,
    activeRequestId: shellProjection.activeRequestId,
    turnState: shellProjection.turnState,
    interruptVisible:
      shellProjection.workflow.kind === "awaitingObservation" ||
      shellProjection.workflow.kind === "turnInProgress",
    activityStatus: shellProjection.activityStatus,
    sendStatus: shellProjection.sendStatus,
    retryStatus: retryShellProjection.nonEmptyContentSendStatus,
    setSelectedAgentDid: selectAgent,
    setSelectedSessionId: selectSession,
    setSelectedBehaviorId: selectBehavior,
    setDraft,
    clearPendingMailboxCause,
    onOpenMailboxItem,
    onDismissMailboxItem,
    onSelectSession,
    onStartNewConversation,
    refreshSession,
    retrySessionHydration,
    loadOlderSessionTimeline,
    refreshSnapshot,
    onRemovePeer,
    onRenamePeer,
    onFetchPeerStatus,
    onRequestStatusEnrollment,
    onInitLocalRuntime,
    onRepairP2P,
    onSendMessage,
    onRetryMessage,
    onRenameConversationTitle,
    onSaveAgentConfig,
    onSaveBehaviorConfig,
    onDeleteSkillConfig,
    onDeleteTaskConfig,
    onDeleteScheduleConfig,
    onDeleteEventTriggerConfig,
    onDeleteBackendConfig,
    onDeleteInferenceProfileConfig,
    onDeleteToolSelectionConfig,
    onDeleteToolServiceConfig,
    onDeleteBehaviorConfig,
    onSaveSkillConfig,
    onSaveBackendConfig,
    onProbeInferenceEndpoint,
    onCodexLogin,
    onCancelCodexLogin,
    onGrokLogin,
    onCancelGrokLogin,
    onSaveInferenceProfileConfig,
    onSaveToolSelectionConfig,
    onSaveToolServiceConfig,
    onTestToolService,
    onSaveTaskConfig,
    onSaveScheduleConfig,
    onRunSchedule,
    onSaveEventTriggerConfig,
    onRunTask,
  };
}
