import {
  selectedBehaviorReadinessDecision,
  type ChatSendResult,
  type DesktopApiAdapter,
  type DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";

import {
  adoptNewChatFolder,
  folderOf,
  withFolder,
  writeChatFolders,
} from "./chatFolders";
import { setterOf } from "./chatStore";
import { actionFailure, shownFailure } from "./desktopShellRuntime";
import { selection } from "./selectionStore";
import type { ShellProjection, ShellStores } from "./shellProjection";

type ChatActionParams = {
  api: DesktopApiAdapter;
  stores: ShellStores;
  /** the shell as the stores hold it now: a send is admitted by it */
  project: () => ShellProjection;
  refreshSession: (
    nextSessionId: string | null,
  ) => Promise<DesktopSessionSnapshot | null>;
  refreshSnapshot: () => Promise<void>;
  setError: (error: string | null) => void;
};

export function releaseOwnedSubmissionWorkflow(
  current: ChatWorkflowState,
  owned: ChatWorkflowState,
): ChatWorkflowState {
  return current === owned ? { kind: "ready" } : current;
}

/**
 * Sending, retrying and renaming in the selected chat. Each reads the
 * selection, the node and the shell projection when it runs.
 */
export function createDesktopShellChatActions({
  api,
  stores,
  project,
  refreshSession,
  refreshSnapshot,
  setError,
}: ChatActionParams) {
  const store = stores.selection;
  const setLocalWorkflow = setterOf(stores.chat, "localWorkflow");
  const setOptimisticPendingTurn = setterOf(stores.chat, "optimisticPendingTurn");
  /* Synchronous admission implements startSubmit before React renders
     sending. Every send and retry entry point shares it. */
  let submissionInFlight = false;

  /** the selected node, or for a send with none selected, the first */
  const selectedNode = () => {
    const fleet = stores.fleet.getState();
    const agentDid = store.getState().agentDid;
    return (agentDid ? fleet.nodes[agentDid] : undefined) ?? null;
  };
  const firstNode = () => {
    const fleet = stores.fleet.getState();
    const first = fleet.nodeKeys[0];
    return (first ? fleet.nodes[first] : undefined) ?? null;
  };

  async function sendMessage(
    content: string,
    behaviorId?: string | null,
  ): Promise<ChatSendResult | null> {
    if (submissionInFlight) return null;
    const node = selectedNode() ?? firstNode();
    if (!node || !content.trim()) return null;

    const projection = project();
    const status = projection.shellProjection.nonEmptyContentSendStatus;
    if (status.kind !== "ready") {
      setError(status.hint);
      return null;
    }
    const admission =
      behaviorId === undefined
        ? projection.behaviorReadiness
        : selectedBehaviorReadinessDecision(node, behaviorId);
    if (admission.kind !== "ready") {
      setError("The selected behavior is unavailable");
      return null;
    }

    submissionInFlight = true;
    const intentGeneration = selection.captureIntent(store);
    const { sessionId: selectedSessionId, mailboxRoute } = store.getState();
    const ownedWorkflow: ChatWorkflowState = {
      kind: "submittingRequest",
      agentDid: node.agentDid,
      sessionId: selectedSessionId,
    };
    /* one write: the app releases a submission whose send has ended, so the
       workflow and the send start together */
    stores.chat.setState({ localWorkflow: ownedWorkflow, sending: true });
    setError(null);
    try {
      const result = await api.sendChatMessage({
        agentDid: node.agentDid,
        behaviorId: admission.behaviorId,
        sessionId: selectedSessionId,
        content,
        causedBySourceDocId: mailboxRoute?.itemId ?? null,
        cwd: folderOf(stores.chat.getState().folders, selectedSessionId),
      });
      if (selectedSessionId === null)
        writeChatFolders(stores.chat, (folders) =>
          adoptNewChatFolder(folders, result.sessionId),
        );
      if (!selection.acceptsIntent(store, intentGeneration)) return result;
      selection.adoptSession(store, result.sessionId);
      setOptimisticPendingTurn({
        sessionId: result.sessionId,
        requestId: result.requestId,
        content,
        selectedSkillIds: [],
        lifecycleState: "pending",
        createdAt: new Date().toISOString(),
      });
      setLocalWorkflow({
        kind: "awaitingObservation",
        agentDid: node.agentDid,
        sessionId: result.sessionId,
        requestId: result.requestId,
      });
      return result;
    } catch (err) {
      if (!selection.acceptsIntent(store, intentGeneration)) return null;
      setLocalWorkflow({ kind: "ready" });
      setError(actionFailure("send the message", err));
      return null;
    } finally {
      stores.chat.setState((state) => ({
        localWorkflow: releaseOwnedSubmissionWorkflow(
          state.localWorkflow,
          ownedWorkflow,
        ),
        sending: false,
      }));
      submissionInFlight = false;
    }
  }

  async function retryMessage(requestId: string) {
    if (submissionInFlight) return;
    const node = selectedNode();
    if (!node) return;
    const status = project().retryShellProjection.nonEmptyContentSendStatus;
    if (status.kind !== "ready") {
      setError(status.hint);
      return;
    }
    submissionInFlight = true;
    const intentGeneration = selection.captureIntent(store);
    const ownedWorkflow: ChatWorkflowState = {
      kind: "submittingRequest",
      agentDid: node.agentDid,
      sessionId: store.getState().sessionId,
    };
    /* one write: the app releases a submission whose send has ended, so the
       workflow and the send start together */
    stores.chat.setState({ localWorkflow: ownedWorkflow, sending: true });
    setError(null);
    try {
      const result = await api.retryRequest(requestId, node.agentDid);
      if (!selection.acceptsIntent(store, intentGeneration)) return;
      selection.settleSession(store, result.sessionId);
      setLocalWorkflow({
        kind: "awaitingObservation",
        agentDid: node.agentDid,
        sessionId: result.sessionId,
        requestId: result.requestId,
      });
    } catch (err) {
      if (!selection.acceptsIntent(store, intentGeneration)) return;
      setLocalWorkflow({ kind: "ready" });
      setError(actionFailure("retry the message", err));
    } finally {
      stores.chat.setState((state) => ({
        localWorkflow: releaseOwnedSubmissionWorkflow(
          state.localWorkflow,
          ownedWorkflow,
        ),
        sending: false,
      }));
      submissionInFlight = false;
    }
  }

  async function renameSession(sessionId: string, title: string) {
    const node = selectedNode();
    if (!node) return;
    setError(null);
    try {
      await api.renameSession({ agentDid: node.agentDid, sessionId, title });
      await refreshSnapshot();
      await refreshSession(sessionId);
    } catch (err) {
      setError(actionFailure("rename the session", err));
      throw shownFailure(err);
    }
  }

  function setChatFolder(folder: string | null) {
    const sessionId = store.getState().sessionId;
    writeChatFolders(stores.chat, (folders) => withFolder(folders, sessionId, folder));
  }

  return {
    /**
     * Sends a message to the selected node (the first node while none is
     * selected), under the selected behavior or the one given, admitted by
     * the shell projection as the stores hold it now. Returns null without
     * sending for empty content or no node, while another send or retry is
     * in flight, or when admission refuses (whose reason is reported).
     * Carries the chat's folder and a held mailbox item as the cause; on
     * acceptance the session becomes the selected one and its turn shows at
     * once, unless the person has moved on.
     */
    sendMessage,
    /**
     * Retries a failed request through the bridge's fenced retry, admitted
     * under the session's own behavior rather than the composer's. Shares
     * sendMessage's single submission in flight and drops its result if the
     * person has moved on. A failure is reported once.
     */
    retryMessage,
    /**
     * Renames a session on the selected node, then reads the client and the
     * session again. A failure is reported once, then rethrown.
     */
    renameSession,
    /**
     * Sets, or clears with null, the folder the selected chat works in; on the
     * new-session screen it is held for the session the first send creates.
     * Kept in this viewer's storage only.
     */
    setChatFolder,
  };
}
