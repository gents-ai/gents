import { useEffect, useMemo, useState } from "react";
import { useStore } from "zustand";

import { reconcileProjectedWorkflow } from "@source-inc/gents-desktop-chat";

import { setterOf } from "./chatStore";
import { createDraftStore } from "./draftStore";
import type { SessionHeader } from "./sessionStore";
import { projectShell, summaryIn, type ShellStores } from "./shellProjection";

type ChatProjectionStateOptions = {
  stores: ShellStores;
  /** the selected session's header, while the held read is the selected one */
  session: SessionHeader | null;
  /** the requests whose user row the transcript holds */
  userRequestIds: ReadonlySet<string>;
};

/** Own local compose state and reconcile it with the bounded durable projection. */
export function useDesktopChatProjectionState({
  stores,
  session,
  userRequestIds,
}: ChatProjectionStateOptions) {
  const selection = useStore(stores.selection);
  const agentDid = selection.agentDid;
  const node = useStore(stores.fleet, (s) =>
    agentDid ? (s.nodes[agentDid] ?? null) : null,
  );
  const sessionSummary = useStore(stores.fleet, (s) =>
    summaryIn(s, agentDid, selection.sessionId),
  );
  const clientAvailable = useStore(stores.client, (s) => Boolean(s.snapshot?.client));
  const syncHealth = useStore(
    stores.client,
    (s) => s.snapshot?.client?.syncHealth ?? null,
  );
  const { localWorkflow, sending, optimisticPendingTurn } = useStore(stores.chat);
  const [setLocalWorkflow] = useState(() => setterOf(stores.chat, "localWorkflow"));

  /* the same pure projection an action reads when it runs */
  const projection = useMemo(
    () =>
      projectShell({
        clientAvailable,
        syncHealth,
        selection,
        node,
        sessionSummary,
        session,
        localWorkflow,
        sending,
      }),
    [
      clientAvailable,
      syncHealth,
      selection,
      node,
      sessionSummary,
      session,
      localWorkflow,
      sending,
    ],
  );

  const [draftStore] = useState(createDraftStore);
  const draftContextKey = JSON.stringify(
    selection.sessionId
      ? ["session", agentDid, selection.sessionId]
      : ["new", agentDid, projection.behaviorReadiness.behaviorId],
  );
  /* the draft itself is read by the composer, not here: the workflow does
     not depend on it, and a keystroke must not re-render the shell */

  useEffect(() => {
    setLocalWorkflow((current) =>
      reconcileProjectedWorkflow(current, projection.shellProjection.workflow),
    );
  }, [projection.shellProjection.workflow, setLocalWorkflow]);

  /* the optimistic turn stands in for a sent message until the transcript
     holds its durable row; derived, so it ends whichever arrives first */
  const visiblePendingTurn =
    optimisticPendingTurn &&
    !(
      optimisticPendingTurn.sessionId === session?.sessionId &&
      userRequestIds.has(optimisticPendingTurn.requestId)
    )
      ? optimisticPendingTurn
      : null;

  return {
    draftStore,
    draftContextKey,
    localWorkflow,
    setLocalWorkflow,
    sending,
    optimisticPendingTurn: visiblePendingTurn,
    operationalState: projection.operationalState,
    behaviorReadiness: projection.behaviorReadiness,
    shellProjection: projection.shellProjection,
    retryShellProjection: projection.retryShellProjection,
    selectedTrackedRequestId: projection.trackedRequestId,
  };
}
