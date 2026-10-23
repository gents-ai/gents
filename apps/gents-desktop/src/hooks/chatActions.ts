import {
  selectedNodeReadinessDecision,
  type ChatSendResult,
  type PendingMessageSteerRequest,
  type PendingQueueEditRequest,
  type DesktopApiAdapter,
  type DesktopInterruptRequestRequest,
  type DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import type { DesktopJobControlRequest } from "@source-inc/gents-desktop-client/generated/DesktopJobControlRequest";
import type { QueueDelivery } from "@source-inc/gents-desktop-client/generated/QueueDelivery";

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
import {
  contentKey,
  forgetUnresolvedSubmission,
  rememberUnresolvedSubmission,
  unresolvedSubmissionFor,
} from "./unresolvedSubmissions";
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

export const DELIVERY_UNKNOWN_HINT =
  "Your message may have been delivered. Check the session before resending; sending the same message again checks delivery instead of creating a duplicate.";
export const EARLIER_SEND_NOT_DELIVERED_HINT =
  "Your earlier message was not delivered. Send it again.";

export function sessionShowsRequest(
  session: DesktopSessionSnapshot | null,
  requestId: string,
): boolean {
  if (!session) return false;
  return (
    session.latestRequestId === requestId ||
    session.pendingTurn?.requestId === requestId ||
    session.queuedTurns.some((turn) => turn.requestId === requestId) ||
    session.foldedInputs.some((input) => input.requestId === requestId) ||
    (session.pendingQueue ?? []).some((entry) => entry.requestId === requestId) ||
    session.timelineItems.some(
      (item) =>
        item.kind === "userMessage" &&
        (item.requestId === requestId || item.inputRequestId === requestId),
    )
  );
}

function queueEditFailure(
  after: DesktopSessionSnapshot | null,
  requestIds: string[],
  error: unknown,
  doing = "change pending messages",
): string {
  if (!after || requestIds.length === 0) return actionFailure(doing, error);
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
  return actionFailure(doing, error);
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
    followUp?: QueueDelivery,
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
    // A send whose delivery is unknown is reconciled, never re-sent under a
    // fresh identity: the owner may already hold the original request.
    let earlier = unresolvedSubmissionFor(node.nodeDid, selectedSessionId, content);
    if (
      earlier &&
      sessionShowsRequest(
        heldFor(readSession(stores.session), earlier.sessionId, node.nodeDid),
        earlier.requestId,
      )
    ) {
      // The session already shows the earlier request, so it was delivered and
      // this identical message is a deliberate new send.
      forgetUnresolvedSubmission(earlier.requestId);
      earlier = undefined;
    }
    chat.beginSubmission(stores.chat, ownedWorkflow);
    try {
      const result = await api.sendChatMessage({
        nodeDid: node.nodeDid,
        agentId: admission.agentId,
        sessionId: earlier?.sessionId ?? selectedSessionId,
        content,
        /* how a message behind a running turn waits; the bridge steers when
           it is not said */
        delivery: status.kind === "queue" ? (followUp ?? null) : null,
        causedBySourceDocId: mailboxRoute?.itemId ?? null,
        cwd: folderOf(stores.chat.getState().folders, selectedSessionId),
        resolveRequestId: earlier?.requestId ?? null,
      });
      if (earlier) forgetUnresolvedSubmission(earlier.requestId);
      if (result.outcome === "notDelivered") {
        // The owner definitely rejected the retained, byte-identical signed
        // request, so it was not delivered.
        if (!selection.acceptsIntent(store, intentGeneration)) return null;
        chat.resetWorkflow(stores.chat);
        reportFailure(EARLIER_SEND_NOT_DELIVERED_HINT);
        return null;
      }
      if (result.outcome === "deliveryUnknown") {
        rememberUnresolvedSubmission({
          nodeDid: result.nodeDid,
          requestId: result.requestId,
          sessionId: result.sessionId,
          selectedSessionId,
          contentKey: contentKey(content),
        });
        reportFailure(DELIVERY_UNKNOWN_HINT);
        if (selection.acceptsIntent(store, intentGeneration))
          chat.resetWorkflow(stores.chat);
        return null;
      }
      if (selectedSessionId === null)
        writeChatFolders(stores.chat, (folders) =>
          adoptNewChatFolder(folders, result.sessionId),
        );
      if (!selection.acceptsIntent(store, intentGeneration)) return result;
      selection.adoptSession(store, result.sessionId);
      chat.showPendingTurn(
        stores.chat,
        {
          sessionId: result.sessionId,
          requestId: result.requestId,
          content,
          selectedSkillIds: [],
          lifecycleState: "pending",
          foldedIntoRequestId: null,
          origin: null,
          createdAt: new Date().toISOString(),
        },
        status.kind === "queue",
      );
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

  async function steerPendingMessage(request: PendingMessageSteerRequest) {
    const before = heldFor(
      readSession(stores.session),
      request.sessionId,
      request.nodeDid,
    );
    const queue = before?.pendingQueue ?? [];
    const at = queue.findIndex((entry) => entry.requestDocId === request.requestDocId);
    const requestIds = queue.slice(0, at + 1).map((entry) => entry.requestId);
    const showing = () =>
      store.getState().nodeDid === request.nodeDid &&
      store.getState().sessionId === request.sessionId;
    try {
      await api.steerPendingMessage(request);
      if (showing()) await refreshSession(request.sessionId);
    } catch (error) {
      const after = showing() ? await refreshSession(request.sessionId) : null;
      reportFailure(
        queueEditFailure(after, at < 0 ? [] : requestIds, error, "steer the message"),
      );
      throw shownFailure(error);
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
    steerPendingMessage,
    retryMessage,
    renameSession,
    interruptRequest,
    backgroundTool: (request: Parameters<DesktopApiAdapter["backgroundTool"]>[0]) =>
      api.backgroundTool(request),
    controlJob: (request: DesktopJobControlRequest) => api.jobControl(request),
    setChatFolder,
  };
}
