import { FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  fetchDesktopSnapshot,
  fetchSessionSnapshot,
  initLocalStandardRuntime,
  renameConversation,
  sendChatMessage,
  shutdownDesktopClient,
  startDesktopClient,
} from "../lib/desktop-api";
import type {
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  InitSummary,
} from "../lib/types";

export function useDesktopShell() {
  const autostartAttempted = useRef(false);
  const [snapshot, setSnapshot] = useState<DesktopClientSnapshot | null>(null);
  const [session, setSession] = useState<DesktopSessionSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [initializing, setInitializing] = useState(false);
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [label, setLabel] = useState("Local Agent");
  const [dangerouslyOverwrite, setDangerouslyOverwrite] = useState(false);
  const [reset, setReset] = useState(false);
  const [selectedAgentDid, setSelectedAgentDid] = useState<string | null>(null);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [selectedBehaviorId, setSelectedBehaviorId] = useState<string | null>(null);
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

  const sessionTools = useMemo(() => {
    if (!session) {
      return [];
    }
    return [...session.toolCalls].sort(
      (left, right) => (left.messageSequence ?? 0) - (right.messageSequence ?? 0),
    );
  }, [session]);

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
      const next = await fetchSessionSnapshot(nextSessionId);
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
    let disposed = false;
    let unlisten: (() => void) | undefined;

    void listen("desktop://client-updated", async () => {
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
  }, [selectedSessionId]);

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
  }, [selectedSessionId]);

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

  async function onSendMessage(event: FormEvent) {
    event.preventDefault();
    if (!selectedDeployment || !draft.trim()) {
      return;
    }

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
      await refreshSnapshot();
      await refreshSession(result.sessionId);
    } catch (err) {
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

  return {
    snapshot,
    session,
    loading,
    initializing,
    starting,
    stopping,
    sending,
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
    sessionTools,
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
    onSendMessage,
    onRenameConversationTitle,
  };
}
