import { useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";

import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
  MailboxItemView,
} from "@source-inc/gents-desktop-client";
import { dismissMailboxItemAndClearMatchingRoute } from "./desktopShellRuntime";

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
  const newConversationAgentRef = useRef<string | null>(null);
  const pendingMailboxRouteRef = useRef<{
    itemId: string;
    agentDid: string;
    behaviorId: string;
    sessionId: string | null;
  } | null>(null);
  const [pendingMailboxCauseId, setPendingMailboxCauseId] = useState<string | null>(
    null,
  );

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
      newConversationAgentRef.current = null;
      setPendingMailboxCauseId(null);
    }
  }, [pendingMailboxCauseId, selectedAgentDid, selectedBehaviorId, selectedSessionId]);

  function clearPendingMailboxCause() {
    pendingMailboxRouteRef.current = null;
    newConversationAgentRef.current = null;
    setPendingMailboxCauseId(null);
  }

  async function onOpenMailboxItem(itemId: string): Promise<MailboxItemView> {
    try {
      const item = await api.startMailboxRequest(itemId);
      pendingMailboxRouteRef.current = {
        itemId: item.itemId,
        agentDid: item.targetAgentDid,
        behaviorId: item.targetBehaviorId,
        sessionId: item.sessionId ?? null,
      };
      newConversationAgentRef.current = item.targetAgentDid;
      setSelectedAgentDid(item.targetAgentDid);
      setSelectedBehaviorId(item.targetBehaviorId);
      setSelectedSessionId(item.sessionId ?? null);
      setSession(null);
      setPendingMailboxCauseId(item.itemId);
      setError(null);
      return item;
    } catch (error) {
      setError(String(error));
      throw error;
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
      setError(String(error));
      throw error;
    }
  }

  function selectAgent(agentDid: string | null) {
    if (agentDid !== selectedAgentDid) clearPendingMailboxCause();
    setSelectedAgentDid(agentDid);
  }

  function selectSession(sessionId: string | null) {
    if (sessionId !== selectedSessionId) clearPendingMailboxCause();
    setSelectedSessionId(sessionId);
  }

  function selectBehavior(behaviorId: string | null) {
    if (behaviorId !== selectedBehaviorId) clearPendingMailboxCause();
    setSelectedBehaviorId(behaviorId);
  }

  return {
    newConversationAgentRef,
    pendingMailboxCauseId,
    setPendingMailboxCauseId,
    clearPendingMailboxCause,
    onOpenMailboxItem,
    onDismissMailboxItem,
    selectAgent,
    selectSession,
    selectBehavior,
  };
}
