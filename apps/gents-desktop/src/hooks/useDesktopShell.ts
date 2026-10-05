import { useMemo, useState, type SetStateAction } from "react";

import { setDesktopShellTimingConfigForTests } from "./desktopShellRuntime";
import { useDesktopShellEffects } from "./desktopShellEffects";
import { useDesktopClientLifecycle } from "./useDesktopClientLifecycle";
import { useDesktopChatProjectionState } from "./useDesktopChatProjectionState";
import { createChatStore } from "./chatStore";
import { createClientStore } from "./clientStore";
import { createFleetStore } from "./fleetStore";
import { createSelectionStore, useSelection } from "./selectionStore";
import { projectShell, projectionInputsOf, type ShellStores } from "./shellProjection";
import {
  createSessionStore,
  headerOf,
  useSessionFields,
  useSessionValue,
  writeSession,
} from "./sessionStore";
import { selectedIn } from "../ui/hooks/useSelectedSession";
import { createSessionReads } from "./sessionReads";
import { createShellActions } from "./shellActions";
import { folderOf } from "./chatFolders";
import { useStore } from "zustand";
import type {
  DesktopApiAdapter,
  DesktopClientUpdatedListenerFactory,
  DesktopSessionSnapshot,
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
  const [error, setError] = useState<string | null>(null);
  /* A failed action is reported once, as a toast, by the action itself:
     it happened where the person clicked and is over. Only the client's own
     state belongs in the banner with Reconnect: the lifecycle, the session
     reads and the effects that refresh in the background, so a repeated
     poll failure does not raise a toast every interval. Actions clear an
     earlier error with null; a toast has nothing to clear. The bridge is
     the app's for its life, so this is made once. */
  const [setActionError] = useState(() => (message: string | null) => {
    if (message) reportFailure?.(message);
  });
  const sessionStore = stores.session;
  const [setSession] = useState(
    () => (next: SetStateAction<DesktopSessionSnapshot | null>) =>
      writeSession(sessionStore, next),
  );
  const [reads] = useState(() =>
    createSessionReads({
      api,
      store,
      sessionStore,
      trackedRequestId,
      setError,
    }),
  );
  const { refreshSession, refreshSessionLiveDelta } = reads;
  const sessionLoad = useSessionValue(sessionStore, (state) => state.load);
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
  /* every action, made once: each reads the stores when it runs */
  const [actions] = useState(() =>
    createShellActions({
      api,
      stores,
      project: projectNow,
      reads,
      client: { refreshSnapshot, mutateSnapshot, ensureDesktopClientStarted },
      setError: setActionError,
    }),
  );
  const { selectAgent } = actions;
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
    localWorkflow,
    setLocalWorkflow,
    sending,
    optimisticPendingTurn,
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

  const chatFolder = useStore(stores.chat, (state) =>
    folderOf(state.folders, selectedSessionId),
  );

  function onDismissError() {
    setError(null);
  }

  return {
    ...actions,
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
    nonEmptyContentSendStatus: shellProjection.nonEmptyContentSendStatus,
    retryStatus: retryShellProjection.nonEmptyContentSendStatus,
    chatFolder,
  };
}
