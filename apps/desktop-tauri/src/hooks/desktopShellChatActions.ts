import type { Dispatch, FormEvent, MutableRefObject, SetStateAction } from "react";

import { renameConversation, sendChatMessage } from "../lib/desktop-api";
import type { ChatShellProjection, ChatWorkflowState } from "../lib/chat-shell";
import type { DeploymentView, DesktopSessionSnapshot } from "../lib/types";

type ChatActionParams = {
  draft: string;
  newConversationAgentRef: MutableRefObject<string | null>;
  refreshSession: (
    nextSessionId: string | null,
  ) => Promise<DesktopSessionSnapshot | null>;
  refreshSnapshot: () => Promise<void>;
  selectedBehaviorId: string | null;
  selectedDeployment: DeploymentView | null;
  selectedSessionId: string | null;
  session: DesktopSessionSnapshot | null;
  setDraft: Dispatch<SetStateAction<string>>;
  setError: Dispatch<SetStateAction<string | null>>;
  setLocalWorkflow: Dispatch<SetStateAction<ChatWorkflowState>>;
  setSelectedBehaviorId: Dispatch<SetStateAction<string | null>>;
  setSelectedSessionId: Dispatch<SetStateAction<string | null>>;
  setSending: Dispatch<SetStateAction<boolean>>;
  setSession: Dispatch<SetStateAction<DesktopSessionSnapshot | null>>;
  shellProjection: ChatShellProjection;
};

export function createDesktopShellChatActions({
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
}: ChatActionParams) {
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
      newConversationAgentRef.current = null;
      setSelectedSessionId(result.sessionId);
      setLocalWorkflow({
        kind: "awaitingObservation",
        sessionId: result.sessionId,
        requestId: result.requestId,
      });
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

  function onSelectSession(sessionId: string) {
    const conversation = selectedDeployment?.conversations.find(
      (conversation) => conversation.sessionId === sessionId,
    );
    if (conversation?.behaviorId) {
      setSelectedBehaviorId(conversation.behaviorId);
    }
    newConversationAgentRef.current = null;
    if (session?.sessionId !== sessionId) {
      setSession(null);
    }
    setSelectedSessionId(sessionId);
  }

  function onStartNewConversation(behaviorId?: string | null) {
    if (!selectedDeployment) {
      return;
    }
    if (
      behaviorId &&
      selectedDeployment.behaviors.some(
        (behavior) => behavior.behaviorId === behaviorId,
      )
    ) {
      setSelectedBehaviorId(behaviorId);
    }
    newConversationAgentRef.current = selectedDeployment.agentDid;
    setSelectedSessionId(null);
    setSession(null);
    setLocalWorkflow({ kind: "ready" });
    setError(null);
  }

  return {
    onRenameConversationTitle,
    onSelectSession,
    onSendMessage,
    onStartNewConversation,
  };
}
