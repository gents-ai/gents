import { useEffect, type MutableRefObject } from "react";

import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import type {
  DeploymentView,
  DesktopApiAdapter,
  DesktopClientUpdatedListenerFactory,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  P2PHealth,
} from "@source-inc/gents-desktop-client";
import { selectedBehaviorIdForDeployment } from "@source-inc/gents-desktop-client";
import { isMacTauriShell, ownsAutomaticRecovery } from "../lib/shellPlatform";
import {
  logShellEvent,
  shouldAutoRestartP2P,
  timingConfig,
} from "./desktopShellRuntime";
import { selection, type SelectionStore } from "./selectionStore";
import { useDesktopProjectionEffects } from "./useDesktopProjectionEffects";

type DesktopShellEffectsArgs = {
  api: DesktopApiAdapter;
  autoRestartInFlight: MutableRefObject<boolean>;
  autostartAttempted: MutableRefObject<boolean>;
  deployments: DeploymentView[];
  lastObservedP2PHealth: MutableRefObject<P2PHealth | null>;
  lastP2PAutoRestartAt: MutableRefObject<number | null>;
  localWorkflow: ChatWorkflowState;
  clientAutostarts: (snapshot: DesktopClientSnapshot) => boolean;
  listenToUpdates: DesktopClientUpdatedListenerFactory;
  /** the node a new session is being composed for, whose behavior is the person's */
  composingFor: string | null;
  refreshSession: (sessionId: string | null) => Promise<DesktopSessionSnapshot | null>;
  refreshSessionLiveDelta: () => Promise<boolean>;
  refreshSnapshot: () => Promise<void>;
  restartDesktopClient: (reason: string) => Promise<void>;
  runtimeHealth: P2PHealth | null;
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  selectedDeployment: DeploymentView | null;
  selectedSessionId: string | null;
  /** the selection, read when an observed result lands */
  store: SelectionStore;
  /** the request being tracked now, read when an update lands */
  trackedRequestId: () => string | null;
  selectedTrackedRequestId: string | null;
  sending: boolean;
  setLocalWorkflow: (workflow: ChatWorkflowState) => void;
  setError: (error: string | null) => void;
  selectAgent: (agentDid: string | null) => void;
  snapshot: DesktopClientSnapshot | null;
  starting: boolean;
  stopping: boolean;
  onStartClient: () => Promise<void>;
};

export function useDesktopShellEffects({
  api,
  autoRestartInFlight,
  autostartAttempted,
  deployments,
  lastObservedP2PHealth,
  lastP2PAutoRestartAt,
  localWorkflow,
  clientAutostarts,
  listenToUpdates,
  composingFor,
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
  onStartClient,
}: DesktopShellEffectsArgs) {
  const clientAvailable = Boolean(snapshot?.client);

  useEffect(() => {
    if (
      !ownsAutomaticRecovery() ||
      !snapshot ||
      snapshot.client ||
      starting ||
      sending
    ) {
      return;
    }

    if (!clientAutostarts(snapshot)) {
      return;
    }

    if (autostartAttempted.current) {
      return;
    }

    autostartAttempted.current = true;
    void onStartClient();
  }, [
    autostartAttempted,
    clientAutostarts,
    onStartClient,
    sending,
    snapshot,
    starting,
  ]);

  useEffect(() => {
    const previousHealth = lastObservedP2PHealth.current;
    lastObservedP2PHealth.current = runtimeHealth;

    if (!runtimeHealth) {
      return;
    }

    if (runtimeHealth.status === "healthy") {
      lastP2PAutoRestartAt.current = null;
      return;
    }

    if (
      !ownsAutomaticRecovery() ||
      autoRestartInFlight.current ||
      starting ||
      stopping ||
      sending ||
      !shouldAutoRestartP2P(
        previousHealth,
        runtimeHealth,
        lastP2PAutoRestartAt.current,
        Date.now(),
        timingConfig().p2pAutoRestartCooldownMs,
      )
    ) {
      return;
    }

    lastP2PAutoRestartAt.current = Date.now();
    logShellEvent(
      `auto restart requested reason="P2P transport wedged" status=${runtimeHealth.status} failures=${runtimeHealth.consecutiveFailures}`,
    );
    void restartDesktopClient("P2P transport wedged");
  }, [
    autoRestartInFlight,
    lastObservedP2PHealth,
    lastP2PAutoRestartAt,
    restartDesktopClient,
    runtimeHealth,
    sending,
    starting,
    stopping,
  ]);

  useDesktopProjectionEffects({
    clientAvailable,
    listenToUpdates,
    refreshSession,
    refreshSessionLiveDelta,
    refreshSnapshot,
    selectedAgentDid,
    selectedSessionId,
    store,
    selectedTrackedRequestId,
    trackedRequestId,
    setError,
  });

  useEffect(() => {
    // Snapshot absence is not an explicit navigation intent. Preserve an
    // existing principal selection while bounded observations catch up; only
    // initialize an empty selection through the route owner.
    if (!selectedAgentDid && deployments.length) {
      selectAgent(deployments[0].agentDid);
    }
  }, [deployments, selectedAgentDid, selectAgent]);

  useEffect(() => {
    if (!clientAvailable) {
      return;
    }

    let disposed = false;
    const publishSelection = () => {
      void api.setSelectedAgent(selectedAgentDid).catch((err) => {
        if (disposed) {
          return;
        }
        setError(String(err));
      });
    };
    publishSelection();
    // Closing siblings returns focus to the surviving view. Republish so the
    // host can narrow observation again after returning to a single window.
    if (isMacTauriShell()) window.addEventListener("focus", publishSelection);

    return () => {
      disposed = true;
      window.removeEventListener("focus", publishSelection);
    };
  }, [api, clientAvailable, selectedAgentDid, setError]);

  useEffect(() => {
    if (!selectedDeployment) {
      selection.settleBehavior(store, null);
      return;
    }

    // A mailbox tap or the new-session screen chose this behavior. Preserve
    // it while the independently replicated behavior and session rows catch
    // up; explicit navigation lets go of it in the selection store.
    if (composingFor === selectedDeployment.agentDid) {
      return;
    }

    const effectiveBehaviorId = selectedBehaviorIdForDeployment(
      selectedDeployment,
      selectedBehaviorId,
    );

    selection.settleBehavior(store, effectiveBehaviorId);

    // A snapshot may reconcile behavior availability, never user session
    // selection. Null is an intentional fresh composer, not a request to open
    // the first matching session. A missing selected row stays selected while
    // hydration/error presentation handles its availability (ClientShell's
    // snapshot_preserves_selection contract).
  }, [composingFor, selectedBehaviorId, selectedDeployment, store]);

  useEffect(() => {
    if (localWorkflow.kind === "submittingRequest" && !sending) {
      setLocalWorkflow({ kind: "ready" });
    }
  }, [localWorkflow, sending, setLocalWorkflow]);
}
