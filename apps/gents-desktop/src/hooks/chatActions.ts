import {
  selectedNodeReadinessDecision,
  type ChatSendResult,
  type PendingQueueEditRequest,
  type DesktopApiAdapter,
  type DesktopInterruptRequestRequest,
  type DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";

import {
  adoptNewChatFolder,
  folderOf,
  withFolder,
  writeChatFolders,
} from "./chatFolders";
import { chat } from "./chatStore";
import { firstNode, nodeOf } from "./fleetStore";
import { actionFailure, shownFailure } from "./actionFailure";
import { selection } from "./selectionStore";
import { heldFor, readSession } from "./sessionStore";
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
  /** shows a failed action to the person, once */
  reportFailure: (message: string) => void;
};

/**
 * Sending, retrying and renaming in the selected chat. Each reads the
 * selection, the node and the shell projection when it runs.
 */
export function createChatActions({
  api,
  stores,
  project,
  refreshSession,
  refreshSnapshot,
  reportFailure,
}: ChatActionParams) {
  const store = stores.selection;
  /* Synchronous admission implements startSubmit before React renders
     sending. Every send and retry entry point shares it. */
  let submissionInFlight = false;

  /** the selected node; a send with none selected goes to the first */
  const selectedNode = () => nodeOf(stores.fleet.getState(), store.getState().nodeDid);

  async function sendMessage(
    content: string,
    agentId?: string | null,
  ): Promise<ChatSendResult | null> {
    if (submissionInFlight) return null;
    const node = selectedNode() ?? firstNode(stores.fleet.getState());
    if (!node || !content.trim()) return null;

    const projection = project();
    const status = projection.shellProjection.nonEmptyContentSendStatus;
    if (status.kind === "disabled") {
      reportFailure(status.hint);
      return null;
    }
    const admission =
      agentId === undefined
        ? projection.agentReadiness
        : selectedNodeReadinessDecision(node, agentId);
    if (admission.kind !== "ready") {
      reportFailure("The selected agent is unavailable");
      return null;
    }

    submissionInFlight = true;
    const intentGeneration = selection.captureIntent(store);
    const { sessionId: selectedSessionId, mailboxRoute } = store.getState();
    const ownedWorkflow: ChatWorkflowState = {
      kind: "submittingRequest",
      nodeDid: node.nodeDid,
      sessionId: selectedSessionId,
    };
    chat.beginSubmission(stores.chat, ownedWorkflow);
    try {
      const result = await api.sendChatMessage({
        nodeDid: node.nodeDid,
        agentId: admission.agentId,
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
      chat.showPendingTurn(stores.chat, {
        sessionId: result.sessionId,
        requestId: result.requestId,
        content,
        selectedSkillIds: [],
        lifecycleState: "pending",
        foldedIntoRequestId: null,
        origin: null,
        createdAt: new Date().toISOString(),
      });
      chat.awaitObservation(stores.chat, {
        nodeDid: node.nodeDid,
        sessionId: result.sessionId,
        requestId: result.requestId,
      });
      return result;
    } catch (err) {
      if (!selection.acceptsIntent(store, intentGeneration)) return null;
      chat.resetWorkflow(stores.chat);
      reportFailure(actionFailure("send the message", err));
      return null;
    } finally {
      chat.endSubmission(stores.chat, ownedWorkflow);
      submissionInFlight = false;
    }
  }

  async function retryMessage(requestId: string) {
    if (submissionInFlight) return;
    const node = selectedNode();
    if (!node) return;
    const status = project().retryShellProjection.nonEmptyContentSendStatus;
    if (status.kind !== "ready") {
      reportFailure(status.hint);
      return;
    }
    submissionInFlight = true;
    const intentGeneration = selection.captureIntent(store);
    const ownedWorkflow: ChatWorkflowState = {
      kind: "submittingRequest",
      nodeDid: node.nodeDid,
      sessionId: store.getState().sessionId,
    };
    chat.beginSubmission(stores.chat, ownedWorkflow);
    try {
      const result = await api.retryRequest(requestId, node.nodeDid);
      if (!selection.acceptsIntent(store, intentGeneration)) return;
      selection.settleSession(store, result.sessionId);
      chat.awaitObservation(stores.chat, {
        nodeDid: node.nodeDid,
        sessionId: result.sessionId,
        requestId: result.requestId,
      });
    } catch (err) {
      if (!selection.acceptsIntent(store, intentGeneration)) return;
      chat.resetWorkflow(stores.chat);
      reportFailure(actionFailure("retry the message", err));
    } finally {
      chat.endSubmission(stores.chat, ownedWorkflow);
      submissionInFlight = false;
    }
  }

  async function editPendingQueue(request: PendingQueueEditRequest) {
    try {
      await api.editPendingQueue(request);
      if (
        store.getState().nodeDid === request.nodeDid &&
        store.getState().sessionId === request.sessionId
      )
        await refreshSession(request.sessionId);
    } catch (error) {
      reportFailure(actionFailure("change pending messages", error));
      throw shownFailure(error);
    }
  }

  async function renameSession(sessionId: string, title: string) {
    /* the selection, not the fleet read: a read can briefly not list the
       node while the session it holds is still shown */
    const nodeDid =
      heldFor(readSession(stores.session), sessionId, null)?.nodeDid ??
      store.getState().nodeDid;
    try {
      if (!nodeDid) throw new Error("no node holds this session");
      await api.renameSession({ nodeDid, sessionId, title });
      await refreshSnapshot();
      await refreshSession(sessionId);
    } catch (err) {
      reportFailure(actionFailure("rename the session", err));
      throw shownFailure(err);
    }
  }

  async function interruptRequest(
    request: Omit<DesktopInterruptRequestRequest, "cause">,
  ) {
    try {
      await api.interruptRequest({ ...request, cause: "userCancelled" });
    } catch (err) {
      reportFailure(actionFailure("stop", err));
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
     * selected), under the selected agent or the one given, admitted by
     * the shell projection as the stores hold it now. Returns null without
     * sending for empty content or no node, while another send or retry is
     * in flight, or when admission refuses (whose reason is reported).
     * Carries the chat's folder and a held mailbox item as the cause; on
     * acceptance the session becomes the selected one and its turn shows at
     * once, unless the person has moved on.
     */
    sendMessage,
    editPendingQueue,
    /**
     * Retries a failed request through the bridge's fenced retry, admitted
     * under the session's own agent rather than the composer's. Shares
     * sendMessage's single submission in flight and drops its result if the
     * person has moved on. A failure is reported once.
     */
    retryMessage,
    /**
     * Renames a session on the node holding it (the selected node), then
     * reads the client and the session again. A failure, or no node to ask,
     * is reported once, then rethrown.
     */
    renameSession,
    /**
     * Stops one request, as the person asked: sessions it started keep their
     * own work. The request's screen settles when it is terminal. A failure
     * is reported once and rethrown.
     */
    interruptRequest,
    /**
     * Sets, or clears with null, the folder the selected chat works in; on the
     * new-session screen it is held for the session the first send creates.
     * Kept in this viewer's storage only.
     */
    setChatFolder,
  };
}
