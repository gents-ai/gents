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
  project: () => ShellProjection;
  refreshSession: (
    nextSessionId: string | null,
  ) => Promise<DesktopSessionSnapshot | null>;
  refreshSnapshot: () => Promise<void>;
  reportFailure: (message: string) => void;
};

function queueEditFailure(
  after: DesktopSessionSnapshot | null,
  requestIds: string[],
  error: unknown,
): string {
  if (!after || requestIds.length === 0)
    return actionFailure("change pending messages", error);
  const waiting = new Set((after.pendingQueue ?? []).map((entry) => entry.requestId));
  const taken = new Set([
    ...after.foldedInputs.map((input) => input.requestId),
    ...(after.latestRequestId ? [after.latestRequestId] : []),
  ]);
  if (requestIds.every((id) => taken.has(id) && !waiting.has(id)))
    return requestIds.length > 1
      ? "These messages already reached the agent"
      : "This message already reached the agent";
  if (requestIds.every((id) => waiting.has(id)))
    return "The queue changed before it was saved";
  return actionFailure("change pending messages", error);
}

export function createChatActions({
  api,
  stores,
  project,
  refreshSession,
  refreshSnapshot,
  reportFailure,
}: ChatActionParams) {
  const store = stores.selection;
  let submissionInFlight = false;

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
    const selected = new Set(request.selectedRequestDocIds);
    const requestIdsIn = (session: DesktopSessionSnapshot | null) =>
      (session?.pendingQueue ?? [])
        .filter((entry) => selected.has(entry.requestDocId))
        .map((entry) => entry.requestId);
    const before = heldFor(
      readSession(stores.session),
      request.sessionId,
      request.nodeDid,
    );
    const showing = () =>
      store.getState().nodeDid === request.nodeDid &&
      store.getState().sessionId === request.sessionId;
    try {
      await api.editPendingQueue(request);
      if (showing()) await refreshSession(request.sessionId);
    } catch (error) {
      const after = showing() ? await refreshSession(request.sessionId) : null;
      reportFailure(queueEditFailure(after, requestIdsIn(before), error));
      throw shownFailure(error);
    }
  }

  async function renameSession(sessionId: string, title: string) {
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
    sendMessage,
    editPendingQueue,
    retryMessage,
    renameSession,
    interruptRequest,
    setChatFolder,
  };
}
