import { useEffect, useMemo, useRef, useState } from "react";

import {
  fetchDesktopSnapshot,
  fetchSessionSnapshot,
  shutdownDesktopClient,
  startDesktopClient,
} from "../lib/desktop-api";
import { listenToDesktopClientUpdates } from "../lib/desktop-events";
import {
  projectChatShell,
  type ChatWorkflowState,
} from "../lib/chat-shell";
import {
  delay,
  logShellEvent,
  setDesktopShellTimingConfigForTests,
  shouldAutoRestartP2P,
  timingConfig,
  trackedRequestIdForSession,
} from "./desktopShellRuntime";
import { createDesktopShellChatActions } from "./desktopShellChatActions";
import { createDesktopShellConfigActions } from "./desktopShellConfigActions";
import { createDesktopShellPeerActions } from "./desktopShellPeerActions";
import { createDesktopShellTaskActions } from "./desktopShellTaskActions";
import type {
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  P2PHealth,
} from "../lib/types";

export { setDesktopShellTimingConfigForTests };

export function useDesktopShell() {
  const autostartAttempted = useRef(false);
  const autoRestartInFlight = useRef(false);
  const lastP2PAutoRestartAt = useRef<number | null>(null);
  const lastObservedP2PHealth = useRef<P2PHealth | null>(null);
  const selectedSessionIdRef = useRef<string | null>(null);
  const newConversationAgentRef = useRef<string | null>(null);
  const [snapshot, setSnapshot] = useState<DesktopClientSnapshot | null>(null);
  const [session, setSession] = useState<DesktopSessionSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [sending, setSending] = useState(false);
  const [savingBehaviorConfig, setSavingBehaviorConfig] = useState(false);
  const [savingConfig, setSavingConfig] = useState(false);
  const [addingPeer, setAddingPeer] = useState(false);
  const [repairingP2P, setRepairingP2P] = useState(false);
  const [runningTask, setRunningTask] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedAgentDid, setSelectedAgentDid] = useState<string | null>(null);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [selectedBehaviorId, setSelectedBehaviorId] = useState<string | null>(null);
  const [localWorkflow, setLocalWorkflow] = useState<ChatWorkflowState>({ kind: "ready" });
  const [draft, setDraft] = useState("");

  const deployments = snapshot?.client?.deployments ?? [];
  const selectedDeployment =
    deployments.find((deployment) => deployment.agentDid === selectedAgentDid) ?? null;
  const selectedConversation =
    selectedDeployment?.conversations.find(
      (conversation) => conversation.sessionId === selectedSessionId,
    ) ?? null;
  const behaviorOptions = selectedDeployment?.behaviors ?? [];
  const runtimeHealth = snapshot?.client?.p2pHealth ?? null;
  const shellProjection = useMemo(
    () =>
      projectChatShell({
        clientAvailable: Boolean(snapshot?.client),
        selectedAgentDid,
        selectedSessionId,
        draft,
        sending,
        session,
        selectedConversation,
        localWorkflow,
      }),
    [
      draft,
      localWorkflow,
      selectedAgentDid,
      selectedConversation,
      selectedSessionId,
      sending,
      session,
      snapshot?.client,
    ],
  );
  const canSendMessage = shellProjection.sendStatus.kind === "ready";

  const selectedTrackedRequestId = trackedRequestIdForSession(
    selectedSessionId,
    shellProjection.workflow,
  );

  useEffect(() => {
    selectedSessionIdRef.current = selectedSessionId;
  }, [selectedSessionId]);

  async function refreshSnapshot() {
    setLoading(true);
    try {
      const next = await fetchDesktopSnapshot();
      setSnapshot(next);
      setError(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  async function refreshSession(nextSessionId: string | null) {
    if (!nextSessionId) {
      setSession(null);
      return;
    }

    try {
      const next = await fetchSessionSnapshot(
        nextSessionId,
        selectedAgentDid,
        trackedRequestIdForSession(nextSessionId, shellProjection.workflow),
      );
      setSession(next);
    } catch (err) {
      setError(String(err));
    }
  }

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
  }, [sending, snapshot, starting]);

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
  }, [runtimeHealth, sending, starting, stopping]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    void listenToDesktopClientUpdates(async () => {
      if (disposed) {
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
  }, [selectedSessionId, selectedTrackedRequestId]);

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
  }, [deployments, selectedAgentDid]);

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
      selectedDeployment.conversations.some(
        (conversation) => conversation.sessionId === selectedSessionId,
      )
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
  }, [selectedDeployment, selectedBehaviorId, selectedSessionId]);

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
  }, [localWorkflow, sending]);

  async function onStartClient() {
    setStarting(true);
    setError(null);
    try {
      const next = await startDesktopClient();
      setSnapshot(next);
    } catch (err) {
      setError(String(err));
    } finally {
      setStarting(false);
    }
  }

  async function restartDesktopClient(reason: string) {
    if (autoRestartInFlight.current) {
      return;
    }

    autoRestartInFlight.current = true;
    const sessionId = selectedSessionIdRef.current;
    logShellEvent(
      `restart begin reason="${reason}" sessionId=${sessionId ?? "none"}`,
    );
    setStopping(true);
    setStarting(true);
    setError(null);

    try {
      let next: DesktopClientSnapshot | null = null;
      for (
        let attempt = 1;
        attempt <= timingConfig().clientRestartMaxAttempts;
        attempt += 1
      ) {
        try {
          logShellEvent(`restart attempt=${attempt} phase=shutdown`);
          await shutdownDesktopClient();
          logShellEvent(`restart attempt=${attempt} phase=start`);
          next = await startDesktopClient();
          logShellEvent(`restart attempt=${attempt} phase=started`);
          break;
        } catch (err) {
          logShellEvent(
            `restart attempt=${attempt} failed error=${String(err)}`,
          );
          if (attempt === timingConfig().clientRestartMaxAttempts) {
            throw err;
          }
          await delay(timingConfig().clientRestartBackoffMs);
        }
      }

      if (!next) {
        throw new Error("desktop restart returned no snapshot");
      }

      setSnapshot(next);
      if (sessionId) {
        await refreshSession(sessionId);
      } else {
        setSession(null);
      }
      logShellEvent(`restart complete reason="${reason}"`);
    } catch (err) {
      logShellEvent(`restart failed reason="${reason}" error=${String(err)}`);
      setError(`desktop client restart failed after ${reason}: ${String(err)}`);
    } finally {
      setStopping(false);
      setStarting(false);
      autoRestartInFlight.current = false;
    }
  }

  const { onAddPeer, onFetchPeerStatus, onRepairP2P } =
    createDesktopShellPeerActions({
      snapshot,
      setAddingPeer,
      setError,
      setRepairingP2P,
      setSelectedAgentDid,
      setSnapshot,
      setStarting,
    });

  const {
    onSaveAgentConfig,
    onSaveBackendConfig,
    onSaveBehaviorConfig,
    onSaveInferenceProfileConfig,
    onSaveToolSelectionConfig,
    onSaveToolServiceConfig,
    onTestToolService,
  } = createDesktopShellConfigActions({
    setError,
    setSavingBehaviorConfig,
    setSavingConfig,
    setSelectedAgentDid,
    setSelectedBehaviorId,
    setSnapshot,
    });

  const {
    onRenameConversationTitle,
    onSelectSession,
    onSendMessage,
    onStartNewConversation,
  } = createDesktopShellChatActions({
    draft,
    newConversationAgentRef,
    refreshSession,
    refreshSnapshot,
    selectedBehaviorId,
    selectedDeployment,
    selectedSessionId,
    session,
    setDraft,
    setError,
    setLocalWorkflow,
    setSelectedBehaviorId,
    setSelectedSessionId,
    setSending,
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
    refreshSession,
    refreshSnapshot,
    setError,
    setRunningTask,
    setSavingConfig,
    setSelectedSessionId,
    setSnapshot,
  });

  return {
    snapshot,
    session,
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
    selectedAgentDid,
    selectedSessionId,
    selectedBehaviorId,
    draft,
    deployments,
    selectedDeployment,
    selectedConversation,
    behaviorOptions,
    runtimeHealth,
    canSendMessage,
    chatWorkflow: shellProjection.workflow,
    sendStatus: shellProjection.sendStatus,
    setSelectedAgentDid,
    setSelectedSessionId,
    setSelectedBehaviorId,
    setDraft,
    onSelectSession,
    onStartNewConversation,
    refreshSnapshot,
    onAddPeer,
    onFetchPeerStatus,
    onRepairP2P,
    onSendMessage,
    onRenameConversationTitle,
    onSaveAgentConfig,
    onSaveBehaviorConfig,
    onSaveBackendConfig,
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
