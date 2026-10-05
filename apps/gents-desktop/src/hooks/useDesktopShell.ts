import { useMemo, useState } from "react";
import { useStore } from "zustand";

import type {
  DesktopApiAdapter,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";

import { projectSessionLoadingStatus } from "../lib/loadingStatus";
import { selectedIn } from "../ui/hooks/useSelectedSession";
import { folderOf } from "./chatFolders";
import { createDesktopApp } from "./desktopApp";
import { useDesktopShellEffects } from "./desktopShellEffects";
import { setDesktopShellTimingConfigForTests } from "./desktopShellRuntime";
import { useSelection } from "./selectionStore";
import { headerOf, useSessionFields, useSessionValue } from "./sessionStore";
import { useDesktopChatProjectionState } from "./useDesktopChatProjectionState";
import { useDesktopClientLifecycle } from "./useDesktopClientLifecycle";

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
  /* the app outside React, made once: the bridge is the app's for its life */
  const [app] = useState(() =>
    createDesktopApp({ api, supportsManagedServer, reportFailure }),
  );
  const { stores, lifecycle, actions } = app;
  const store = stores.selection;
  const current = useSelection(store);
  const selectedAgentDid = current.agentDid;
  const selectedSessionId = current.sessionId;
  const selectedBehaviorId = current.behaviorId;
  const pendingMailboxCauseId = current.mailboxRoute?.itemId ?? null;
  const sessionStore = stores.session;
  const sessionLoad = useSessionValue(sessionStore, (state) => state.load);
  const {
    snapshot,
    error,
    startupPhase,
    starting,
    stopping,
    incompatibleHome,
    managedServerWait,
    diagnosticsHint,
    canRestartManagedServer,
  } = useDesktopClientLifecycle(api, stores.client, lifecycle);
  const { selectAgent, refreshSession, refreshSessionLiveDelta, refreshSnapshot } =
    actions;
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
    recovery: lifecycle.recovery,
    deployments,
    localWorkflow,
    clientAutostarts: lifecycle.clientAutostarts,
    listenToUpdates,
    composingFor: current.composingFor,
    onStartClient: lifecycle.onStartClient,
    refreshSession,
    refreshSessionLiveDelta,
    refreshSnapshot,
    restartDesktopClient: lifecycle.restartDesktopClient,
    runtimeHealth,
    selectedAgentDid,
    selectedBehaviorId,
    selectedDeployment,
    selectedSessionId,
    store,
    trackedRequestId: app.trackedRequestId,
    selectedTrackedRequestId,
    sending,
    setLocalWorkflow,
    setError: lifecycle.setError,
    selectAgent,
    snapshot,
    starting,
    stopping,
  });

  const chatFolder = useStore(stores.chat, (state) =>
    folderOf(state.folders, selectedSessionId),
  );

  function onDismissError() {
    lifecycle.setError(null);
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
    onRetryStartup: lifecycle.onRetryStartup,
    incompatibleHome,
    managedServerWait,
    diagnosticsHint,
    onSkipManagedServerWait: lifecycle.onSkipManagedServerWait,
    canRestartManagedServer,
    onRestartManagedServer: lifecycle.onRestartManagedServer,
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
