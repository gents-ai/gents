import { useEffect, useRef, useState } from "react";

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

export { setDesktopShellTimingConfigForTests };
export type { DesktopStartupPhase } from "./useDesktopClientLifecycle";

export type DesktopShellBridge = {
  api: DesktopApiAdapter;
  listenToUpdates: DesktopClientUpdatedListenerFactory;
};

export function useDesktopShell({ api, listenToUpdates }: DesktopShellBridge) {
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
    setSession,
    refreshSession,
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
  const behaviorOptions = selectedDeployment?.behaviors ?? [];
  const runtimeHealth = snapshot?.client?.p2pHealth ?? null;
  const {
    draft,
    setDraft,
    localWorkflow,
    setLocalWorkflow,
    optimisticPendingTurn,
    setOptimisticPendingTurn,
    shellProjection,
    selectedTrackedRequestId,
  } = useDesktopChatProjectionState({
    clientAvailable: Boolean(snapshot?.client),
    selectedAgentDid,
    selectedBehaviorId,
    selectedConversation,
    selectedDeployment,
    selectedSessionId,
    sending,
    session,
  });
  const canSendMessage = shellProjection.sendStatus.kind === "ready";
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
    onAddPeer,
    onFetchPeerStatus,
    onProbePeerAddress,
    onInitLocalRuntime,
    onPairBearer,
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
    draft,
    newConversationAgentRef,
    refreshSession,
    refreshSnapshot,
    selectedBehaviorId,
    selectedDeployment,
    selectedSessionId,
    pendingMailboxCauseId,
    session,
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
    session,
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
    canSendMessage,
    chatWorkflow: shellProjection.workflow,
    activeRequestId: shellProjection.activeRequestId,
    turnState: shellProjection.turnState,
    interruptVisible:
      shellProjection.workflow.kind === "awaitingObservation" ||
      shellProjection.workflow.kind === "turnInProgress",
    sendStatus: shellProjection.sendStatus,
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
    loadOlderSessionTimeline,
    refreshSnapshot,
    onAddPeer,
    onPairBearer,
    onRemovePeer,
    onRenamePeer,
    onFetchPeerStatus,
    onProbePeerAddress,
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
