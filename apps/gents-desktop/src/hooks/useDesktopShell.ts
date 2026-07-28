import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type SetStateAction,
} from "react";

import {
  isTerminalTurnState,
  projectChatShell,
  reconcileProjectedWorkflow,
  type ChatWorkflowState,
} from "@source-inc/gents-desktop-chat";
import {
  delay,
  logShellEvent,
  setDesktopShellTimingConfigForTests,
  timingConfig,
  trackedRequestIdForSession,
} from "./desktopShellRuntime";
import { createDesktopShellChatActions } from "./desktopShellChatActions";
import { createDesktopShellConfigActions } from "./desktopShellConfigActions";
import { useDesktopShellEffects } from "./desktopShellEffects";
import { createDesktopShellPeerActions } from "./desktopShellPeerActions";
import { createDesktopShellTaskActions } from "./desktopShellTaskActions";
import type {
  DesktopApiAdapter,
  DesktopClientUpdatedListenerFactory,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  P2PHealth,
} from "@source-inc/gents-desktop-client";

export { setDesktopShellTimingConfigForTests };

export type DesktopShellBridge = {
  api: DesktopApiAdapter;
  listenToUpdates: DesktopClientUpdatedListenerFactory;
};

export function useDesktopShell({ api, listenToUpdates }: DesktopShellBridge) {
  const autostartAttempted = useRef(false);
  const autoRestartInFlight = useRef(false);
  const lastP2PAutoRestartAt = useRef<number | null>(null);
  const lastObservedP2PHealth = useRef<P2PHealth | null>(null);
  const selectedSessionIdRef = useRef<string | null>(null);
  const selectedAgentDidRef = useRef<string | null>(null);
  const selectedTrackedRequestIdRef = useRef<string | null>(null);
  const snapshotRefreshSeq = useRef(0);
  const sessionRefreshSeq = useRef(0);
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
  const [localWorkflow, setLocalWorkflow] = useState<ChatWorkflowState>({
    kind: "ready",
  });
  const [draftsByContext, setDraftsByContext] = useState<Record<string, string>>({});

  const deployments = snapshot?.client?.deployments ?? [];
  const selectedDeployment =
    deployments.find((deployment) => deployment.agentDid === selectedAgentDid) ?? null;
  const draftContextKey = JSON.stringify(
    selectedSessionId
      ? ["session", selectedAgentDid, selectedSessionId]
      : [
          "new",
          selectedAgentDid,
          selectedBehaviorId ?? selectedDeployment?.defaultBehaviorId ?? null,
        ],
  );
  const draft = draftsByContext[draftContextKey] ?? "";
  const setDraft = useCallback(
    (next: SetStateAction<string>) => {
      setDraftsByContext((current) => {
        const currentDraft = current[draftContextKey] ?? "";
        const nextDraft = typeof next === "function" ? next(currentDraft) : next;
        if (nextDraft === currentDraft) {
          return current;
        }
        if (!nextDraft) {
          const remaining = { ...current };
          delete remaining[draftContextKey];
          return remaining;
        }
        return { ...current, [draftContextKey]: nextDraft };
      });
    },
    [draftContextKey],
  );
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

  useEffect(() => {
    setLocalWorkflow((current) =>
      reconcileProjectedWorkflow(current, shellProjection.workflow),
    );
  }, [shellProjection.workflow]);

  const selectedTrackedRequestId =
    trackedRequestIdForSession(selectedSessionId, shellProjection.workflow) ??
    (!isTerminalTurnState(shellProjection.turnState)
      ? shellProjection.activeRequestId
      : null);
  selectedAgentDidRef.current = selectedAgentDid;
  selectedSessionIdRef.current = selectedSessionId;
  selectedTrackedRequestIdRef.current = selectedTrackedRequestId;

  async function refreshSnapshot() {
    const refreshSeq = snapshotRefreshSeq.current + 1;
    snapshotRefreshSeq.current = refreshSeq;
    setLoading(true);
    try {
      const next = await api.fetchDesktopSnapshot();
      if (snapshotRefreshSeq.current === refreshSeq) {
        setSnapshot(next);
        setError(null);
      }
    } catch (err) {
      if (snapshotRefreshSeq.current === refreshSeq) {
        setError(String(err));
      }
    } finally {
      if (snapshotRefreshSeq.current === refreshSeq) {
        setLoading(false);
      }
    }
  }

  async function refreshSession(
    nextSessionId: string | null,
  ): Promise<DesktopSessionSnapshot | null> {
    const refreshSeq = sessionRefreshSeq.current + 1;
    sessionRefreshSeq.current = refreshSeq;

    if (!nextSessionId) {
      if (sessionRefreshSeq.current === refreshSeq) {
        setSession(null);
      }
      return null;
    }

    try {
      const next = await api.fetchSessionSnapshot(
        nextSessionId,
        selectedAgentDidRef.current,
        selectedTrackedRequestIdRef.current,
      );
      if (sessionRefreshSeq.current === refreshSeq) {
        setSession(next);
      }
      return next;
    } catch (err) {
      if (sessionRefreshSeq.current === refreshSeq) {
        setError(String(err));
      }
      return null;
    }
  }

  async function onStartClient() {
    setStarting(true);
    setError(null);
    try {
      const next = await api.startDesktopClient();
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
    logShellEvent(`restart begin reason="${reason}" sessionId=${sessionId ?? "none"}`);
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
          await api.shutdownDesktopClient();
          logShellEvent(`restart attempt=${attempt} phase=start`);
          next = await api.startDesktopClient();
          logShellEvent(`restart attempt=${attempt} phase=started`);
          break;
        } catch (err) {
          logShellEvent(`restart attempt=${attempt} failed error=${String(err)}`);
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

  useDesktopShellEffects({
    api,
    autoRestartInFlight,
    autostartAttempted,
    deployments,
    lastObservedP2PHealth,
    lastP2PAutoRestartAt,
    localWorkflow,
    listenToUpdates,
    newConversationAgentRef,
    onStartClient,
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
    activeRequestId: shellProjection.activeRequestId,
    turnState: shellProjection.turnState,
    interruptVisible:
      shellProjection.workflow.kind === "awaitingObservation" ||
      shellProjection.workflow.kind === "turnInProgress",
    sendStatus: shellProjection.sendStatus,
    setSelectedAgentDid,
    setSelectedSessionId,
    setSelectedBehaviorId,
    setDraft,
    onSelectSession,
    onStartNewConversation,
    refreshSession,
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
