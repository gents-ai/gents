import { FormEvent, useEffect, useMemo, useRef, useState } from "react";

import {
  addPeer,
  fetchDesktopSnapshot,
  fetchSessionSnapshot,
  initLocalStandardRuntime,
  renameConversation,
  repairP2P,
  runSchedule,
  runTask,
  saveAgentConfig,
  saveBackendConfig,
  saveBehaviorConfig,
  saveEventTriggerConfig,
  saveInferenceProfileConfig,
  saveScheduleConfig,
  saveTaskConfig,
  saveToolSelectionConfig,
  saveToolServiceConfig,
  sendChatMessage,
  shutdownDesktopClient,
  startDesktopClient,
  testToolService,
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
import type {
  AgentConfigSaveRequest,
  BackendSaveRequest,
  BehaviorSaveRequest,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  EventTriggerSaveRequest,
  InferenceProfileSaveRequest,
  InitSummary,
  P2PHealth,
  PeerAddRequest,
  ScheduleRunRequest,
  ScheduleSaveRequest,
  TaskRunRequest,
  TaskRunResult,
  TaskSaveRequest,
  ToolSelectionSaveRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
} from "../lib/types";

export { setDesktopShellTimingConfigForTests };

export function useDesktopShell() {
  const autostartAttempted = useRef(false);
  const autoRestartInFlight = useRef(false);
  const lastP2PAutoRestartAt = useRef<number | null>(null);
  const lastObservedP2PHealth = useRef<P2PHealth | null>(null);
  const selectedSessionIdRef = useRef<string | null>(null);
  const [snapshot, setSnapshot] = useState<DesktopClientSnapshot | null>(null);
  const [session, setSession] = useState<DesktopSessionSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [initializing, setInitializing] = useState(false);
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [sending, setSending] = useState(false);
  const [savingBehaviorConfig, setSavingBehaviorConfig] = useState(false);
  const [savingConfig, setSavingConfig] = useState(false);
  const [addingPeer, setAddingPeer] = useState(false);
  const [repairingP2P, setRepairingP2P] = useState(false);
  const [runningTask, setRunningTask] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [label, setLabel] = useState("Local Agent");
  const [dangerouslyOverwrite, setDangerouslyOverwrite] = useState(false);
  const [reset, setReset] = useState(false);
  const [selectedAgentDid, setSelectedAgentDid] = useState<string | null>(null);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [selectedBehaviorId, setSelectedBehaviorId] = useState<string | null>(null);
  const [localWorkflow, setLocalWorkflow] = useState<ChatWorkflowState>({ kind: "ready" });
  const [draft, setDraft] = useState("");
  const [initSummary, setInitSummary] = useState<InitSummary | null>(null);

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
    if (!snapshot || snapshot.client || starting || initializing || sending) {
      return;
    }

    if (autostartAttempted.current) {
      return;
    }

    autostartAttempted.current = true;
    void onStartClient();
  }, [initializing, sending, snapshot, starting]);

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
      initializing ||
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
  }, [initializing, runtimeHealth, sending, starting, stopping]);

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

  async function onInit(event: FormEvent) {
    event.preventDefault();
    setInitializing(true);
    setError(null);
    try {
      const result = await initLocalStandardRuntime({
        label,
        dangerouslyOverwrite,
        reset,
      });
      setInitSummary(result);
      await refreshSnapshot();
      autostartAttempted.current = false;
      await onStartClient();
    } catch (err) {
      setError(String(err));
    } finally {
      setInitializing(false);
    }
  }

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

  async function onShutdownClient() {
    setStopping(true);
    setError(null);
    try {
      const next = await shutdownDesktopClient();
      setSnapshot(next);
      setSession(null);
      setSelectedSessionId(null);
      autostartAttempted.current = true;
    } catch (err) {
      setError(String(err));
    } finally {
      setStopping(false);
    }
  }

  async function onAddPeer(request: PeerAddRequest) {
    setAddingPeer(true);
    setError(null);
    try {
      const next = await addPeer(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setAddingPeer(false);
    }
  }

  async function onRepairP2P() {
    setRepairingP2P(true);
    setError(null);
    try {
      const next = await repairP2P();
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setRepairingP2P(false);
    }
  }

  async function onSendMessage(event: FormEvent) {
    event.preventDefault();
    if (!selectedDeployment || !draft.trim()) {
      return;
    }

    if (shellProjection.sendStatus.kind !== "ready") {
      setError(shellProjection.sendStatus.hint);
      return;
    }

    setLocalWorkflow({
      kind: "submittingRequest",
      agentDid: selectedDeployment.agentDid,
      sessionId: selectedSessionId,
    });
    setSending(true);
    setError(null);
    try {
      const result = await sendChatMessage({
        agentDid: selectedDeployment.agentDid,
        behaviorId: selectedBehaviorId,
        sessionId: selectedSessionId,
        content: draft,
      });
      setDraft("");
      setSelectedSessionId(result.sessionId);
      setLocalWorkflow({
        kind: "awaitingObservation",
        sessionId: result.sessionId,
        requestId: result.requestId,
      });
      await refreshSnapshot();
      await refreshSession(result.sessionId);
    } catch (err) {
      setLocalWorkflow({ kind: "ready" });
      setError(String(err));
    } finally {
      setSending(false);
    }
  }

  async function onRenameConversationTitle(sessionId: string, title: string) {
    setError(null);
    try {
      await renameConversation({ sessionId, title });
      await refreshSnapshot();
      await refreshSession(sessionId);
    } catch (err) {
      setError(String(err));
      throw err;
    }
  }

  async function onSaveAgentConfig(request: AgentConfigSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveAgentConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
      setSelectedBehaviorId(request.defaultBehaviorId);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveBehaviorConfig(request: BehaviorSaveRequest) {
    setSavingBehaviorConfig(true);
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveBehaviorConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
      setSelectedBehaviorId(request.behaviorId);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingBehaviorConfig(false);
      setSavingConfig(false);
    }
  }

  async function onSaveBackendConfig(request: BackendSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveBackendConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveInferenceProfileConfig(
    request: InferenceProfileSaveRequest,
  ) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveInferenceProfileConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveToolSelectionConfig(request: ToolSelectionSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveToolSelectionConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveToolServiceConfig(request: ToolServiceSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveToolServiceConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onTestToolService(
    request: ToolServiceTestRequest,
  ): Promise<ToolServiceTestResult> {
    setError(null);
    try {
      return await testToolService(request);
    } catch (err) {
      setError(String(err));
      throw err;
    }
  }

  async function onSaveTaskConfig(request: TaskSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveTaskConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveScheduleConfig(request: ScheduleSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveScheduleConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onRunSchedule(request: ScheduleRunRequest): Promise<TaskRunResult> {
    setRunningTask(true);
    setError(null);
    try {
      const result = await runSchedule(request);
      await refreshSnapshot();
      if (result.sessionId) {
        setSelectedSessionId(result.sessionId);
        await refreshSession(result.sessionId);
      }
      return result;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setRunningTask(false);
    }
  }

  async function onSaveEventTriggerConfig(request: EventTriggerSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveEventTriggerConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onRunTask(request: TaskRunRequest): Promise<TaskRunResult> {
    setRunningTask(true);
    setError(null);
    try {
      const result = await runTask(request);
      await refreshSnapshot();
      if (result.sessionId) {
        setSelectedSessionId(result.sessionId);
        await refreshSession(result.sessionId);
      }
      return result;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setRunningTask(false);
    }
  }

  return {
    snapshot,
    session,
    loading,
    initializing,
    starting,
    stopping,
    sending,
    savingBehaviorConfig,
    savingConfig,
    addingPeer,
    repairingP2P,
    runningTask,
    error,
    label,
    dangerouslyOverwrite,
    reset,
    selectedAgentDid,
    selectedSessionId,
    selectedBehaviorId,
    draft,
    initSummary,
    deployments,
    selectedDeployment,
    selectedConversation,
    behaviorOptions,
    runtimeHealth,
    canSendMessage,
    chatWorkflow: shellProjection.workflow,
    sendStatus: shellProjection.sendStatus,
    setLabel,
    setDangerouslyOverwrite,
    setReset,
    setSelectedAgentDid,
    setSelectedSessionId,
    setSelectedBehaviorId,
    setDraft,
    refreshSnapshot,
    onInit,
    onStartClient,
    onShutdownClient,
    onAddPeer,
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
