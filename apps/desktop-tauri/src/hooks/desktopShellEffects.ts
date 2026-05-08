import { useEffect, type MutableRefObject } from "react";

import type { ChatWorkflowState } from "../lib/chat-shell";
import type {
  DeploymentView,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  P2PHealth,
} from "../lib/types";
import {
  logShellEvent,
  shouldAutoRestartP2P,
  timingConfig,
} from "./desktopShellRuntime";
import { setSelectedAgent } from "../lib/desktop-api";
import { listenToDesktopClientUpdates } from "../lib/desktop-events";

type DesktopShellEffectsArgs = {
  autoRestartInFlight: MutableRefObject<boolean>;
  autostartAttempted: MutableRefObject<boolean>;
  deployments: DeploymentView[];
  lastObservedP2PHealth: MutableRefObject<P2PHealth | null>;
  lastP2PAutoRestartAt: MutableRefObject<number | null>;
  localWorkflow: ChatWorkflowState;
  newConversationAgentRef: MutableRefObject<string | null>;
  refreshSession: (sessionId: string | null) => Promise<DesktopSessionSnapshot | null>;
  refreshSnapshot: () => Promise<void>;
  restartDesktopClient: (reason: string) => Promise<void>;
  runtimeHealth: P2PHealth | null;
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  selectedDeployment: DeploymentView | null;
  selectedSessionId: string | null;
  selectedSessionIdRef: MutableRefObject<string | null>;
  selectedTrackedRequestId: string | null;
  sending: boolean;
  setLocalWorkflow: (workflow: ChatWorkflowState) => void;
  setError: (error: string | null) => void;
  setSelectedAgentDid: (agentDid: string | null) => void;
  setSelectedBehaviorId: (behaviorId: string | null) => void;
  setSelectedSessionId: (sessionId: string | null) => void;
  snapshot: DesktopClientSnapshot | null;
  starting: boolean;
  stopping: boolean;
  onStartClient: () => Promise<void>;
};

function isTerminalTurnState(turnState?: string | null) {
  return (
    turnState === "completed" ||
    turnState === "failed" ||
    turnState === "superseded" ||
    turnState === "interrupted"
  );
}

export function useDesktopShellEffects({
  autoRestartInFlight,
  autostartAttempted,
  deployments,
  lastObservedP2PHealth,
  lastP2PAutoRestartAt,
  localWorkflow,
  newConversationAgentRef,
  refreshSession,
  refreshSnapshot,
  restartDesktopClient,
  runtimeHealth,
  selectedAgentDid,
  selectedBehaviorId,
  selectedDeployment,
  selectedSessionId,
  selectedSessionIdRef,
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
  onStartClient,
}: DesktopShellEffectsArgs) {
  const clientAvailable = Boolean(snapshot?.client);

  useEffect(() => {
    selectedSessionIdRef.current = selectedSessionId;
  }, [selectedSessionId, selectedSessionIdRef]);

  useEffect(() => {
    void refreshSnapshot();
  }, []);

  useEffect(() => {
    if (!snapshot || snapshot.client || starting || sending) {
      return;
    }

    if (!snapshot.bootstrap.savedPeers.length) {
      return;
    }

    if (autostartAttempted.current) {
      return;
    }

    autostartAttempted.current = true;
    void onStartClient();
  }, [autostartAttempted, onStartClient, sending, snapshot, starting]);

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

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    void listenToDesktopClientUpdates(async (event) => {
      if (disposed) {
        return;
      }
      if (event.reason === "store" && selectedAgentDid) {
        if (selectedSessionId) {
          const nextSession = await refreshSession(selectedSessionId);
          if (isTerminalTurnState(nextSession?.turnState)) {
            await refreshSnapshot();
          }
        }
        return;
      }
      await refreshSnapshot();
      if (selectedSessionId) {
        await refreshSession(selectedSessionId);
      }
    }).then((cleanup) => {
      if (disposed) {
        cleanup();
        return;
      }
      unlisten = cleanup;
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [selectedAgentDid, selectedSessionId, selectedTrackedRequestId]);

  useEffect(() => {
    if (!deployments.length) {
      setSelectedAgentDid(null);
      return;
    }

    if (
      selectedAgentDid &&
      deployments.some((deployment) => deployment.agentDid === selectedAgentDid)
    ) {
      return;
    }

    setSelectedAgentDid(deployments[0].agentDid);
  }, [deployments, selectedAgentDid, setSelectedAgentDid]);

  useEffect(() => {
    if (!clientAvailable) {
      return;
    }

    let disposed = false;
    void setSelectedAgent(selectedAgentDid).catch((err) => {
      if (disposed) {
        return;
      }
      setError(String(err));
    });

    return () => {
      disposed = true;
    };
  }, [clientAvailable, selectedAgentDid, setError]);

  useEffect(() => {
    if (!selectedDeployment) {
      setSelectedBehaviorId(null);
      setSelectedSessionId(null);
      return;
    }

    const defaultBehaviorId =
      selectedDeployment.defaultBehaviorId ??
      selectedDeployment.behaviors.find((behavior) => behavior.isDefault)?.behaviorId ??
      selectedDeployment.behaviors[0]?.behaviorId ??
      null;

    if (
      !selectedBehaviorId ||
      !selectedDeployment.behaviors.some(
        (behavior) => behavior.behaviorId === selectedBehaviorId,
      )
    ) {
      setSelectedBehaviorId(defaultBehaviorId);
    }

    if (
      selectedSessionId &&
      (selectedDeployment.conversations.some(
        (conversation) => conversation.sessionId === selectedSessionId,
      ) ||
        (localWorkflow.kind === "awaitingObservation" &&
          localWorkflow.sessionId === selectedSessionId))
    ) {
      newConversationAgentRef.current = null;
      return;
    }

    if (
      !selectedSessionId &&
      newConversationAgentRef.current === selectedDeployment.agentDid
    ) {
      return;
    }

    setSelectedSessionId(selectedDeployment.conversations[0]?.sessionId ?? null);
  }, [
    localWorkflow,
    newConversationAgentRef,
    selectedBehaviorId,
    selectedDeployment,
    selectedSessionId,
    setSelectedBehaviorId,
    setSelectedSessionId,
  ]);

  useEffect(() => {
    void refreshSession(selectedSessionId);
  }, [selectedSessionId, selectedTrackedRequestId]);

  useEffect(() => {
    if (
      localWorkflow.kind === "submittingRequest" &&
      !sending
    ) {
      setLocalWorkflow({ kind: "ready" });
    }
  }, [localWorkflow, sending, setLocalWorkflow]);
}
