import {
  asBridgeErrorPayload,
  type ChatSendRequest,
  type ChatSendResult,
  type DesktopApiAdapter,
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
import { deliveryAffordances, sendStatus } from "./chatDeliveryView";
import type { ShellStores } from "./shellProjection";

type Applied = (result: ChatSendResult, draft: ChatDraft) => Promise<boolean>;

/** The optimistic send (Lean `ClientOptimisticSend`): the message gets its
 * request identity and a kept payload before dispatch, is pending while the
 * one signed create is in flight, and is confirmed by its reply or failed
 * with Retry, which resends the same payload under the same identity. */
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
  function inFlight(requestId: string) {
    return stores.chat.getState().chatRequestsInFlight.has(requestId);
  }
  function track(requestId: string, running: boolean) {
    stores.chat.setState((state) => {
      const chatRequestsInFlight = new Set(state.chatRequestsInFlight);
      if (running) chatRequestsInFlight.add(requestId);
      else chatRequestsInFlight.delete(requestId);
      return { chatRequestsInFlight };
    });
  }
  async function send(draft: ChatDraft, applied: Applied) {
    if (clientDid() !== draft.clientDid)
      throw new Error("The selected client changed before dispatch.");
    track(draft.requestId, true);
    try {
      let result: ChatSendResult;
      try {
        result = await api.sendChatMessage(chatDraftRequest(draft));
        validateChatResult(draft, result);
      } catch (error) {
        const payload = asBridgeErrorPayload(error);
        saveChatDraft(stores.chat, {
          ...draft,
          failure: {
            message:
              payload?.message ??
              (error instanceof Error ? error.message : String(error)),
            error: payload,
          },
        });
        throw error;
      }
      await applied(result, draft);
      // Confirmed: the payload is released. A mailbox answer stays saved,
      // confirmed, until its question is observed closed.
      if (draft.mailbox)
        saveChatDraft(stores.chat, { ...draft, confirmed: true, failure: undefined });
      else removeChatDraft(stores.chat, draft);
      return result;
    } finally {
      track(draft.requestId, false);
    }
  }
  return {
    async submit(
      request: ChatSendRequest,
      selectedSessionId: string | null,
      queued: boolean,
      applied: Applied,
      mailbox?: ChatDraft["mailbox"],
    ) {
      const actor = clientDid();
      if (request.clientDid && request.clientDid !== actor)
        throw new Error("The selected client changed before dispatch.");
      if (
        chatDraftsFor(
          load(),
          actor,
          request.nodeDid,
          selectedSessionId,
          mailbox?.itemId,
        ).some(
          (draft) =>
            deliveryAffordances(
              sendStatus(draft, inFlight(draft.requestId)),
              mailbox !== undefined,
            ).blocksComposer,
        )
      )
        throw new Error(
          "Retry or discard the saved answer before answering this question again.",
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
        mailbox,
      );
      saveChatDraft(stores.chat, draft);
      return send(draft, applied);
    },
    /** Resend a failed message: the same payload under the same identity. */
    async retry(requestId: string, applied: Applied) {
      const draft = original(requestId);
      if (inFlight(requestId)) throw new Error("This message is still being sent.");
      if (draft.confirmed) throw new Error("This message was already delivered.");
      return send({ ...draft, failure: undefined }, applied);
    },
    /** Discard a failed message. It sends nothing; if an earlier send of it
     * reached the runtime, it still appears in the session. */
    async discard(requestId: string) {
      const draft = original(requestId);
      if (inFlight(requestId)) throw new Error("This message is still being sent.");
      removeChatDraft(stores.chat, draft);
    },
  };
}
