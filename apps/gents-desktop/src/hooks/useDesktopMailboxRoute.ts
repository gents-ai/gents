import { useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";

import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
  MailboxItemView,
  MailboxQuestionAnswer,
} from "@source-inc/gents-desktop-client";
import {
  acceptsAsyncResult,
  actionFailure,
  dismissMailboxItemAndClearMatchingRoute,
  shownFailure,
} from "./desktopShellRuntime";

type MailboxRouteOptions = {
  api: DesktopApiAdapter;
  refreshSnapshot: () => Promise<void>;
  selectedAgentDid: string | null;
  selectedBehaviorId: string | null;
  selectedSessionId: string | null;
  setError: (error: string | null) => void;
  setSelectedAgentDid: Dispatch<SetStateAction<string | null>>;
  setSelectedBehaviorId: Dispatch<SetStateAction<string | null>>;
  setSelectedSessionId: Dispatch<SetStateAction<string | null>>;
  setSession: (next: SetStateAction<DesktopSessionSnapshot | null>) => void;
};

/** Own the exact mailbox-to-compose route while replicated rows catch up. */
export function useDesktopMailboxRoute({
  api,
  refreshSnapshot,
  selectedAgentDid,
  selectedBehaviorId,
  selectedSessionId,
  setError,
  setSelectedAgentDid,
  setSelectedBehaviorId,
  setSelectedSessionId,
  setSession,
}: MailboxRouteOptions) {
  const newSessionAgentRef = useRef<string | null>(null);
  const composeIntentGenerationRef = useRef(0);
  const pendingMailboxRouteRef = useRef<{
    itemId: string;
    agentDid: string;
    behaviorId: string;
    sessionId: string | null;
  } | null>(null);
  const [pendingMailboxCauseId, setPendingMailboxCauseId] = useState<string | null>(
    null,
  );

  function advanceComposeIntent() {
    composeIntentGenerationRef.current += 1;
  }

  function captureComposeIntent() {
    return composeIntentGenerationRef.current;
  }

  function acceptsComposeIntent(capturedGeneration: number) {
    return acceptsAsyncResult(composeIntentGenerationRef.current, capturedGeneration);
  }

  useEffect(() => {
    if (!pendingMailboxCauseId) {
      pendingMailboxRouteRef.current = null;
      return;
    }
    const route = pendingMailboxRouteRef.current;
    if (
      !route ||
      route.itemId !== pendingMailboxCauseId ||
      route.agentDid !== selectedAgentDid ||
      route.behaviorId !== selectedBehaviorId ||
      route.sessionId !== selectedSessionId
    ) {
      pendingMailboxRouteRef.current = null;
      newSessionAgentRef.current = null;
      setPendingMailboxCauseId(null);
    }
  }, [pendingMailboxCauseId, selectedAgentDid, selectedBehaviorId, selectedSessionId]);

  function clearPendingMailboxCause() {
    pendingMailboxRouteRef.current = null;
    newSessionAgentRef.current = null;
    setPendingMailboxCauseId(null);
  }

  async function onOpenMailboxItem(itemId: string): Promise<MailboxItemView | null> {
    advanceComposeIntent();
    const capturedGeneration = captureComposeIntent();
    try {
      const item = await api.startMailboxRequest(itemId);
      if (!acceptsComposeIntent(capturedGeneration)) return null;
      pendingMailboxRouteRef.current = {
        itemId: item.itemId,
        agentDid: item.targetAgentDid,
        behaviorId: item.targetBehaviorId,
        sessionId: item.sessionId ?? null,
      };
      newSessionAgentRef.current = item.targetAgentDid;
      setSelectedAgentDid(item.targetAgentDid);
      setSelectedBehaviorId(item.targetBehaviorId);
      setSelectedSessionId(item.sessionId ?? null);
      setSession(null);
      setPendingMailboxCauseId(item.itemId);
      setError(null);
      return item;
    } catch (error) {
      if (!acceptsComposeIntent(capturedGeneration)) return null;
      setError(actionFailure("open the item", error));
      throw shownFailure(error);
    }
  }

  async function onDismissMailboxItem(itemId: string) {
    try {
      await dismissMailboxItemAndClearMatchingRoute(
        itemId,
        (dismissedItemId) => api.dismissMailboxItem(dismissedItemId),
        () => pendingMailboxRouteRef.current?.itemId ?? null,
        clearPendingMailboxCause,
      );
      await refreshSnapshot();
    } catch (error) {
      setError(actionFailure("dismiss the item", error));
      throw shownFailure(error);
    }
  }

  /* The answer is the item's ordinary reply request; the bridge renders its
     content from the question so the runtime reply claim consumes the item. */
  async function onAnswerMailboxQuestion(
    item: MailboxItemView,
    answer: MailboxQuestionAnswer,
  ) {
    try {
      await api.sendChatMessage({
        agentDid: item.targetAgentDid,
        behaviorId: item.targetBehaviorId,
        sessionId: item.sessionId ?? null,
        content: "",
        causedBySourceDocId: item.itemId,
        answer,
      });
      /* the reply consumed the item, so a compose route opened on it must
         not carry it as the next message's source */
      if (pendingMailboxRouteRef.current?.itemId === item.itemId) {
        clearPendingMailboxCause();
      }
      setError(null);
      await refreshSnapshot();
    } catch (error) {
      setError(actionFailure("send the answer", error));
      throw shownFailure(error);
    }
  }

  function selectAgent(agentDid: string | null) {
    if (agentDid !== selectedAgentDid) {
      advanceComposeIntent();
      clearPendingMailboxCause();
      // Explicit principal navigation owns this reset. Snapshot reconciliation
      // must not guess a replacement session for the newly selected agent.
      setSelectedSessionId(null);
      setSelectedBehaviorId(null);
      setSession(null);
    }
    setSelectedAgentDid(agentDid);
  }

  function selectSession(sessionId: string | null) {
    if (sessionId !== selectedSessionId) {
      advanceComposeIntent();
      clearPendingMailboxCause();
    }
    setSelectedSessionId(sessionId);
  }

  function selectBehavior(behaviorId: string | null) {
    if (behaviorId !== selectedBehaviorId) {
      advanceComposeIntent();
      clearPendingMailboxCause();
    }
    setSelectedBehaviorId(behaviorId);
  }

  return {
    newSessionAgentRef,
    advanceComposeIntent,
    captureComposeIntent,
    acceptsComposeIntent,
    pendingMailboxCauseId,
    setPendingMailboxCauseId,
    clearPendingMailboxCause,
    onOpenMailboxItem,
    onDismissMailboxItem,
    onAnswerMailboxQuestion,
    selectAgent,
    selectSession,
    selectBehavior,
  };
}
