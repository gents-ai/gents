import type { SetStateAction } from "react";

import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import {
  selectedBehaviorIdForDeployment,
  type DeploymentView,
  type DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

import { selection, type SelectionStore } from "./selectionStore";

type SelectionActionParams = {
  store: SelectionStore;
  /** the nodes as last read, for a session's node and behavior */
  deployments: () => readonly DeploymentView[];
  setSession: (next: SetStateAction<DesktopSessionSnapshot | null>) => void;
  setLocalWorkflow: (workflow: ChatWorkflowState) => void;
  setError: (error: string | null) => void;
};

/**
 * Every way the person moves between nodes, sessions and behaviors. Each
 * reads the selection when it runs, so none holds a stale copy, and each
 * drops the session the screen showed when it no longer applies.
 */
export function createDesktopShellSelectionActions({
  store,
  deployments,
  setSession,
  setLocalWorkflow,
  setError,
}: SelectionActionParams) {
  const selectedDeployment = () =>
    deployments().find((d) => d.agentDid === store.getState().agentDid) ?? null;

  /** Explicit node navigation resets its session; a snapshot never guesses
      a replacement session for a newly selected node. */
  function selectAgent(agentDid: string | null) {
    if (selection.selectAgent(store, agentDid)) setSession(null);
  }

  function selectBehavior(behaviorId: string | null) {
    selection.selectBehavior(store, behaviorId);
  }

  /** A session on the selected node. Selection is behavior-aware: reopening
      an older session restores the behavior it was held under. */
  function selectSession(sessionId: string) {
    const listed = selectedDeployment()?.sessions.find(
      (s) => s.sessionId === sessionId,
    );
    selection.selectSession(store, sessionId, listed?.behaviorId);
    if (!listed) setSession(null);
  }

  /** The new-session screen on the selected node (or the first one), with
      the behavior asked for or the node's default. */
  function startNewSession(behaviorId?: string | null) {
    const deployment = selectedDeployment() ?? deployments()[0] ?? null;
    if (!deployment) return;
    selection.startNewSession(
      store,
      deployment.agentDid,
      selectedBehaviorIdForDeployment(deployment, behaviorId ?? null),
    );
    setSession(null);
    setLocalWorkflow({ kind: "ready" });
    setError(null);
  }

  /**
   * The selection a route asks for: the new-session screen for null, else
   * that session, on the node that lists it. A session already selected is
   * left alone: the route is catching up with a selection made a moment
   * ago, such as a mailbox item that opened its session, and selecting it
   * again would reset what was set up for it.
   *
   * Called again whenever a snapshot lands (`fromSnapshot`), since a session
   * on another node can only be followed once that node lists it; then it
   * acts only on a session some node lists, and never restarts the
   * new-session screen.
   */
  function followRoute(sessionId: string | null, fromSnapshot = false) {
    const state = store.getState();
    if (sessionId === null) {
      /* a mailbox item opened into a new session already holds it, with
         the item as the next message's cause */
      if (fromSnapshot || (state.sessionId === null && state.mailboxRoute)) return;
      startNewSession();
      return;
    }
    const owner = deployments().find((d) =>
      d.sessions.some((s) => s.sessionId === sessionId),
    );
    if (owner && owner.agentDid !== state.agentDid) {
      const listed = owner.sessions.find((s) => s.sessionId === sessionId);
      selection.selectSessionOn(store, owner.agentDid, sessionId, listed?.behaviorId);
      setSession(null);
      return;
    }
    if (!owner && fromSnapshot) return;
    if (state.sessionId !== sessionId) selectSession(sessionId);
  }

  return { selectAgent, selectBehavior, selectSession, startNewSession, followRoute };
}
