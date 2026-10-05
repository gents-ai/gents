import { useState } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import type {
  DesktopApiAdapter,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";

import { folderOf } from "./chatFolders";
import { createDesktopApp } from "./desktopApp";
import { useDesktopShellEffects } from "./desktopShellEffects";
import { setDesktopShellTimingConfigForTests } from "./desktopShellRuntime";
import { useSelection } from "./selectionStore";
import { setterOf } from "./chatStore";
import { useSessionValue } from "./sessionStore";
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
  const behaviorOptions = selectedDeployment?.behaviors ?? [];
  const runtimeHealth = snapshot?.client?.p2pHealth ?? null;
  const [setLocalWorkflow] = useState(() => setterOf(stores.chat, "localWorkflow"));
  const { localWorkflow, sending } = useStore(
    stores.chat,
    useShallow((state) => ({
      localWorkflow: state.localWorkflow,
      sending: state.sending,
    })),
  );
  const {
    operationalState,
    behaviorReadiness,
    shellProjection,
    retryShellProjection,
    trackedRequestId: selectedTrackedRequestId,
    loadingStatus: sessionLoadingStatus,
    pendingTurn: optimisticPendingTurn,
    draftKey: draftContextKey,
  } = useStore(app.view);
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
    projectedWorkflow: shellProjection.workflow,
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
    draftStore: app.drafts,
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
