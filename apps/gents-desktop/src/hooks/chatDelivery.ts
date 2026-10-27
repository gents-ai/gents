import type {
  ChatSendRequest,
  ChatSendResult,
  DesktopApiAdapter,
} from "@source-inc/gents-desktop-client";
import {
  chatDraftRequest,
  chatDraftsFor,
  loadChatDrafts,
  removeChatDraft,
  retainChatDraft,
  saveChatDraft,
  validateChatResult,
  type ChatDraft,
} from "./chatDrafts";
import type { ShellStores } from "./shellProjection";

type Applied = (result: ChatSendResult, draft: ChatDraft) => Promise<boolean>;

export function createChatDelivery(api: DesktopApiAdapter, stores: ShellStores) {
  function clientDid() {
    const actor = stores.client.getState().snapshot?.client?.clientDid;
    if (!actor) throw new Error("Connect this client before sending a message.");
    return actor;
  }
  function load() {
    const state = loadChatDrafts();
    stores.chat.setState(state);
    if (state.chatDraftError) throw new Error(state.chatDraftError);
    return state.chatDrafts;
  }
  function original(requestId: string) {
    const draft = load().find(
      (row) => row.requestId === requestId && row.clientDid === clientDid(),
    );
    if (!draft) throw new Error("The original message is unavailable for this client.");
    return draft;
  }
  function track(requestId: string, running: boolean) {
    stores.chat.setState((state) => {
      const chatRequestsInFlight = new Set(state.chatRequestsInFlight);
      if (running) chatRequestsInFlight.add(requestId);
      else chatRequestsInFlight.delete(requestId);
      return { chatRequestsInFlight };
    });
  }
  async function settle(draft: ChatDraft, result: ChatSendResult, applied: Applied) {
    validateChatResult(draft, result);
    saveChatDraft(stores.chat, { ...draft, result });
    if (result.outcome === "deliveryUnknown") return result;
    const consumed =
      result.outcome !== "notDelivered" && (await applied(result, draft));
    void Promise.resolve()
      .then(() => api.acknowledgeChatSubmission(result.submissionReceipt!))
      .then(() => {
        if (consumed) removeChatDraft(stores.chat, draft);
      })
      .catch(() => {});
    return result;
  }
  async function query(draft: ChatDraft) {
    const recovered = await api.recoverChatSubmission({
      clientDid: draft.clientDid,
      nodeDid: draft.nodeDid,
      requestId: draft.requestId,
    });
    if (recovered.kind === "unconfirmed") {
      if (
        recovered.nodeDid !== draft.nodeDid ||
        recovered.requestId !== draft.requestId
      )
        throw new Error("The delivery check belongs to another saved message.");
    } else validateChatResult(draft, recovered.result);
    return recovered;
  }
  return {
    async submit(
      request: ChatSendRequest,
      selectedSessionId: string | null,
      queued: boolean,
      applied: Applied,
    ) {
      const actor = clientDid();
      if (request.clientDid && request.clientDid !== actor)
        throw new Error("The selected client changed before dispatch.");
      if (
        chatDraftsFor(load(), actor, request.nodeDid, selectedSessionId).some(
          (draft) => !draft.result || draft.result.outcome === "deliveryUnknown",
        )
      )
        throw new Error(
          "Check the saved message before sending another message in this chat.",
        );
      const draft = retainChatDraft(
        {
          ...request,
          clientDid: actor,
          requestId: crypto.randomUUID(),
          sessionId: request.sessionId ?? crypto.randomUUID(),
        },
        selectedSessionId,
        queued,
      );
      track(draft.requestId, true);
      try {
        saveChatDraft(stores.chat, draft);
        if (clientDid() !== actor)
          throw new Error("The selected client changed before dispatch.");
        return await settle(
          draft,
          await api.sendChatMessage(chatDraftRequest(draft)),
          applied,
        );
      } finally {
        track(draft.requestId, false);
      }
    },
    async recover(requestId: string, resume: boolean, applied: Applied) {
      let draft = original(requestId);
      if (stores.chat.getState().chatRequestsInFlight.has(requestId))
        throw new Error("This message is still being submitted or checked.");
      track(requestId, true);
      try {
        const recovered = await query(draft);
        if (recovered.kind === "prepared") {
          if (recovered.result.outcome !== "deliveryUnknown")
            return await settle(draft, recovered.result, applied);
          draft = { ...draft, result: recovered.result };
          saveChatDraft(stores.chat, draft);
        }
        if (!resume) return null;
        if (clientDid() !== draft.clientDid)
          throw new Error(
            "The selected client changed while delivery was being checked.",
          );
        if (recovered.kind === "unconfirmed" && draft.result?.submissionReceipt)
          throw new Error(
            "The original signed preparation is unavailable. Reconnect this history before resuming; no new message was submitted.",
          );
        return await settle(
          draft,
          await api.sendChatMessage(chatDraftRequest(draft)),
          applied,
        );
      } finally {
        track(requestId, false);
      }
    },
    async dismiss(requestId: string) {
      const draft = original(requestId);
      const recovered = await query(draft);
      if (recovered.kind !== "prepared" || recovered.result.outcome !== "notDelivered")
        throw new Error(
          "Check delivery before dismissing this message; non-delivery is not confirmed.",
        );
      removeChatDraft(stores.chat, draft);
      void Promise.resolve()
        .then(() => api.acknowledgeChatSubmission(recovered.result.submissionReceipt!))
        .catch(() => {});
    },
  };
}
