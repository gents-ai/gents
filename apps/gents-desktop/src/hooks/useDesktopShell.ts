import { useCallback, useMemo, useRef, useState } from "react";

import { setDesktopShellTimingConfigForTests } from "./desktopShellRuntime";
import { createDesktopShellChatActions } from "./desktopShellChatActions";
import { createDesktopShellConfigActions } from "./desktopShellConfigActions";
import { useDesktopShellEffects } from "./desktopShellEffects";
import { useChatFolders } from "./useChatFolders";
import { useDesktopClientLifecycle } from "./useDesktopClientLifecycle";
import { useDesktopChatProjectionState } from "./useDesktopChatProjectionState";
import { createDesktopShellMailboxActions } from "./desktopShellMailboxActions";
import { createDesktopShellSelectionActions } from "./desktopShellSelectionActions";
import { createChatStore } from "./chatStore";
import { createClientStore } from "./clientStore";
import { createFleetStore } from "./fleetStore";
import { createSelectionStore, selection, useSelection } from "./selectionStore";
import { projectShell, projectionInputsOf, type ShellStores } from "./shellProjection";
import {
  createSessionStore,
  headerOf,
  useSessionFields,
  useSessionValue,
} from "./sessionStore";
import { selectedIn } from "../ui/hooks/useSelectedSession";
import { useDesktopSessionProjection } from "./useDesktopSessionProjection";
import { createDesktopShellPeerActions } from "./desktopShellPeerActions";
import { createDesktopShellTaskActions } from "./desktopShellTaskActions";
import type {
  DesktopApiAdapter,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";
import { projectSessionLoadingStatus } from "../lib/loadingStatus";

export { setDesktopShellTimingConfigForTests };
export type { DesktopStartupPhase } from "./useDesktopClientLifecycle";

export type DesktopShellBridge = {
  api: DesktopApiAdapter;
  listenToUpdates: DesktopClientUpdatedListenerFactory;
  supportsManagedServer?: boolean;
  /** shows a failed action to the person, once; the app passes its toast */
  reportFailure?: (message: string) => void;
};

export function useDesktopShell({
  api,
  listenToUpdates,
  supportsManagedServer = false,
  reportFailure,
}: DesktopShellBridge) {
  /* every store the shell is projected from, created once: screens select
     from them, and actions read them when they run */
  const [stores] = useState<ShellStores>(() => ({
    selection: createSelectionStore(),
    session: createSessionStore(),
    fleet: createFleetStore(),
    client: createClientStore(
      supportsManagedServer ? "checking-managed-server" : "loading-configuration",
    ),
    chat: createChatStore(),
  }));
  /* the projection as the stores hold it now, for an action or a read */
  const [projectNow] = useState(() => () => projectShell(projectionInputsOf(stores)));
  const [trackedRequestId] = useState(() => () => projectNow().trackedRequestId);
  const store = stores.selection;
  const current = useSelection(store);
  const selectedAgentDid = current.agentDid;
  const selectedSessionId = current.sessionId;
  const selectedBehaviorId = current.behaviorId;
  const pendingMailboxCauseId = current.mailboxRoute?.itemId ?? null;
  const captureComposeIntent = () => selection.captureIntent(store);
  const acceptsComposeIntent = (captured: number) =>
    selection.acceptsIntent(store, captured);
  const submissionInFlight = useRef(false);
  const [error, setError] = useState<string | null>(null);
  // A failed action is reported once, as a toast, by the action itself:
  // it happened where the person clicked and is over. Only the client's own
  // state belongs in the banner with Reconnect: the lifecycle, the session
  // reads and the effects that refresh in the background, so a repeated
  // poll failure does not raise a toast every interval. Actions clear an
  // earlier error with null; a toast has nothing to clear.
  const setActionError = useCallback(
    (message: string | null) => {
      if (message) reportFailure?.(message);
    },
    [reportFailure],
  );
  const {
    sessionStore,
    sessionLoad,
    setSession,
    refreshSession,
    retrySessionHydration,
    refreshSessionLiveDelta,
    loadOlderSessionTimeline,
  } = useDesktopSessionProjection({
    api,
    store,
    sessionStore: stores.session,
    trackedRequestId,
    setError,
  });
  const {
    autostartAttempted,
    clientAutostarts,
    autoRestartInFlight,
    lastP2PAutoRestartAt,
    lastObservedP2PHealth,
    snapshot,
    mutateSnapshot,
    startupPhase,
    starting,
    setStarting,
    stopping,
    refreshSnapshot,
    ensureDesktopClientStarted,
    onStartClient,
    onRetryStartup,
    incompatibleHome,
    managedServerWait,
    diagnosticsHint,
    onSkipManagedServerWait,
    canRestartManagedServer,
    onRestartManagedServer,
    restartDesktopClient,
  } = useDesktopClientLifecycle({
    api,
    supportsManagedServer,
    refreshSession,
    store,
    client: stores.client,
    fleet: stores.fleet,
    setError,
    setSession,
  });
  const { onOpenMailboxItem, onDismissMailboxItem, onAnswerMailboxQuestion } =
    createDesktopShellMailboxActions({
      api,
      store,
      refreshSnapshot,
      setError: setActionError,
      setSession,
    });
  const deployments = snapshot?.client?.deployments ?? [];
  const selectedDeployment =
    deployments.find((deployment) => deployment.agentDid === selectedAgentDid) ?? null;
  const selectedSessionSummary =
    selectedDeployment?.sessions.find(
      (session) => session.sessionId === selectedSessionId,
    ) ?? null;
  /* the selected session's header and transcript facts: a streamed chunk
     changes neither, so it does not re-render the shell */
  const selectedSessionHeader = useSessionFields(sessionStore, (state) =>
    headerOf(selectedIn(state, { selectedSessionId, selectedAgentDid })),
  );
  const userRequestIds = useSessionValue(
    sessionStore,
    (state) => state.facts.userRequestIds,
  );
  const behaviorOptions = selectedDeployment?.behaviors ?? [];
  const runtimeHealth = snapshot?.client?.p2pHealth ?? null;
  const {
    draftStore,
    draftContextKey,
    readCurrentDraft,
    setDraft,
    localWorkflow,
    setLocalWorkflow,
    sending,
    setSending,
    optimisticPendingTurn,
    setOptimisticPendingTurn,
    operationalState,
    behaviorReadiness,
    shellProjection,
    retryShellProjection,
    selectedTrackedRequestId,
  } = useDesktopChatProjectionState({
    stores,
    session: selectedSessionHeader,
    userRequestIds,
  });
  const sessionLoadingStatus = useMemo(
    () =>
      projectSessionLoadingStatus({
        selectedSessionId,
        selectedAgentDid,
        session: selectedSessionHeader,
        sessionLoad,
        operationalState,
      }),
    [
      operationalState,
      selectedAgentDid,
      selectedSessionId,
      selectedSessionHeader,
      sessionLoad,
    ],
  );
  const { selectAgent, selectBehavior, selectSession, startNewSession, followRoute } =
    createDesktopShellSelectionActions({
      store,
      deployments: () => deployments,
      setSession,
      setLocalWorkflow,
      setError: setActionError,
    });

  useDesktopShellEffects({
    api,
    autoRestartInFlight,
    autostartAttempted,
    deployments,
    lastObservedP2PHealth,
    lastP2PAutoRestartAt,
    localWorkflow,
    clientAutostarts,
    listenToUpdates,
    composingFor: current.composingFor,
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
    store,
    trackedRequestId,
    selectedTrackedRequestId,
    sending,
    setLocalWorkflow,
    setError,
    selectAgent,
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
  } = createDesktopShellPeerActions({
    api,
    mutateSnapshot,
    refreshSnapshot,
    snapshot,
    ensureDesktopClientStarted,
    setError: setActionError,
    store,
    selectAgent,
    setStarting,
  });
  const { chatFolder, setChatFolder, adoptChatFolder } =
    useChatFolders(selectedSessionId);

  const {
    onSaveAgentConfig,
    onSetDefaultBehavior,
    onSaveBackendConfig,
    onPatchConfigComponents,
    onApplyConfigComponents,
    onSaveBehaviorConfig,
    onDeleteSkillConfig,
    onDeleteContextConfig,
    onDeleteTaskConfig,
    onDeleteScheduleConfig,
    onDeleteEventSourceConfig,
    onDeleteTriggerConfig,
    onDeleteBackendConfig,
    onDeleteInferenceProfileConfig,
    onDeleteToolsConfig,
    onDeleteToolServiceConfig,
    onDeleteBehaviorConfig,
    onProbeInferenceEndpoint,
    onCodexLogin,
    onCancelCodexLogin,
    onGrokLogin,
    onCancelGrokLogin,
    onSaveInferenceProfileConfig,
    onSaveSkillConfig,
    onSaveToolsConfig,
    onSaveToolServiceConfig,
    onTestToolService,
  } = createDesktopShellConfigActions({
    api,
    mutateSnapshot,
    setError: setActionError,
  });

  const { submitContent, onRenameSessionTitle, onRetryMessage, onSendMessage } =
    createDesktopShellChatActions({
      submissionInFlight,
      store,
      api,
      behaviorReadiness,
      chatFolder,
      adoptChatFolder,
      readDraft: readCurrentDraft,
      refreshSession,
      refreshSnapshot,
      selectedDeployment,
      deployments,
      setDraft,
      setError: setActionError,
      setLocalWorkflow,
      setOptimisticPendingTurn,
      setSending,
      shellProjection,
      retryShellProjection,
    });

  const {
    onRunSchedule,
    onRunTask,
    onSaveEventSourceConfig,
    onSaveScheduleConfig,
    onSaveTaskConfig,
    onSaveTriggerConfig,
  } = createDesktopShellTaskActions({
    acceptsComposeIntent,
    api,
    mutateSnapshot,
    captureComposeIntent,
    refreshSnapshot,
    setError: setActionError,
  });

  function onDismissError() {
    setError(null);
  }

  return {
    snapshot,
    fleet: stores.fleet,
    sessionStore,
    sessionLoad,
    sessionLoadingStatus,
    optimisticPendingTurn,
    startupPhase,
    starting,
    stopping,
    sending,
    error,
    onDismissError,
    onRetryStartup,
    incompatibleHome,
    managedServerWait,
    diagnosticsHint,
    onSkipManagedServerWait,
    canRestartManagedServer,
    onRestartManagedServer,
    selectedAgentDid,
    selectedSessionId,
    selectedBehaviorId,
    pendingMailboxCauseId,
    draftStore,
    draftContextKey,
    deployments,
    selectedDeployment,
    selectedSessionSummary,
    behaviorOptions,
    runtimeHealth,
    operationalState,
    behaviorReadiness,
    chatWorkflow: shellProjection.workflow,
    activeRequestId: shellProjection.activeRequestId,
    selectedTrackedRequestId,
    turnState: shellProjection.turnState,
    interruptVisible:
      shellProjection.workflow.kind === "awaitingObservation" ||
      shellProjection.workflow.kind === "turnInProgress",
    activityStatus: shellProjection.activityStatus,
    submitContent,
    captureComposeIntent,
    acceptsComposeIntent,
    nonEmptyContentSendStatus: shellProjection.nonEmptyContentSendStatus,
    retryStatus: retryShellProjection.nonEmptyContentSendStatus,
    selectAgent,
    selectSession,
    selectBehavior,
    startNewSession,
    followRoute,
    setDraft,
    chatFolder,
    setChatFolder,
    clearPendingMailboxCause: () => selection.releaseMailboxRoute(store),
    onOpenMailboxItem,
    onDismissMailboxItem,
    onAnswerMailboxQuestion,
    refreshSession,
    retrySessionHydration,
    loadOlderSessionTimeline,
    refreshSnapshot,
    onRemovePeer,
    onRenamePeer,
    onFetchPeerStatus,
    onRequestStatusEnrollment,
    onInitLocalRuntime,
    onSendMessage,
    onRetryMessage,
    onRenameSessionTitle,
    onSaveAgentConfig,
    onSetDefaultBehavior,
    onSaveBehaviorConfig,
    onDeleteSkillConfig,
    onDeleteContextConfig,
    onDeleteTaskConfig,
    onDeleteScheduleConfig,
    onDeleteEventSourceConfig,
    onDeleteTriggerConfig,
    onDeleteBackendConfig,
    onDeleteInferenceProfileConfig,
    onDeleteToolsConfig,
    onDeleteToolServiceConfig,
    onDeleteBehaviorConfig,
    onSaveSkillConfig,
    onSaveBackendConfig,
    onPatchConfigComponents,
    onApplyConfigComponents,
    onProbeInferenceEndpoint,
    onCodexLogin,
    onCancelCodexLogin,
    onGrokLogin,
    onCancelGrokLogin,
    onSaveInferenceProfileConfig,
    onSaveToolsConfig,
    onSaveToolServiceConfig,
    onTestToolService,
    onSaveTaskConfig,
    onSaveScheduleConfig,
    onRunSchedule,
    onSaveEventSourceConfig,
    onSaveTriggerConfig,
    onRunTask,
  };
}
