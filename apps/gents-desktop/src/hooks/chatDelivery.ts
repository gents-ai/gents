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
  savedChatDraft,
  validateChatResult,
  type ChatDraft,
} from "./chatDrafts";
import { deliveryAffordances, sendStatus } from "./chatDeliveryView";
import type { ShellStores } from "./shellProjection";

type Applied = (result: ChatSendResult, draft: ChatDraft) => Promise<boolean>;

/** The client deadline of one send (Lean `ClientOptimisticSend.timeout`).
 * Neither the bridge nor the runtime bounds a create, so a send with no reply
 * by then shows failed with Retry; a reply that arrives later is still
 * applied, because Retry resends the same identity. */
export const CHAT_SEND_DEADLINE_MS = 30_000;

/** The reply was a confirmation, but the chat view could not show it. The
 * message stays confirmed; only the view update failed. */
export class ChatViewUpdateError extends Error {
  constructor(
    readonly result: ChatSendResult,
    cause: unknown,
  ) {
    super(
      `The message was sent, but this chat could not show it yet: ${
        cause instanceof Error ? cause.message : String(cause)
      }`,
    );
    this.name = "ChatViewUpdateError";
  }
}

type Reply = { ok: true; result: ChatSendResult } | { ok: false; error: unknown };

/** The optimistic send (Lean `ClientOptimisticSend`): the message gets its
 * request identity and a kept payload before dispatch, is pending until its
 * saved deadline while the one signed create is in flight, and is confirmed by
 * its reply or failed with Retry, which resends the same payload under the
 * same identity. The saved draft is the message's only state, shared by every
 * window. */
export function createChatDelivery(
  api: DesktopApiAdapter,
  stores: ShellStores,
  now: () => number = Date.now,
) {
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
  /** Lean `receive`: a confirmation confirms whatever the message shows
   * unless it was discarded; a failure fails only a pending message. */
  async function settle(draft: ChatDraft, reply: Reply, applied: Applied) {
    const current = savedChatDraft(draft);
    if (!reply.ok) {
      if (current && sendStatus(current, now()) === "pending") {
        const payload = asBridgeErrorPayload(reply.error);
        saveChatDraft(stores.chat, {
          ...current,
          pendingUntil: undefined,
          failure: {
            message:
              payload?.message ??
              (reply.error instanceof Error
                ? reply.error.message
                : String(reply.error)),
            error: payload,
          },
        });
      } else stores.chat.setState(loadChatDrafts());
      throw reply.error;
    }
    if (!current) {
      stores.chat.setState(loadChatDrafts());
      return reply.result;
    }
    // Confirmed before the view applies it: a view failure never shows a
    // confirmed message as failed. A mailbox answer stays saved, confirmed,
    // until its question is observed closed.
    if (current.mailbox)
      saveChatDraft(stores.chat, {
        ...current,
        confirmed: true,
        pendingUntil: undefined,
        failure: undefined,
      });
    else removeChatDraft(stores.chat, current);
    try {
      await applied(reply.result, current);
    } catch (error) {
      throw new ChatViewUpdateError(reply.result, error);
    }
    return reply.result;
  }
  async function send(draft: ChatDraft, applied: Applied) {
    if (clientDid() !== draft.clientDid)
      throw new Error("The selected client changed before dispatch.");
    const attempt: ChatDraft = {
      ...draft,
      failure: undefined,
      pendingUntil: now() + CHAT_SEND_DEADLINE_MS,
    };
    saveChatDraft(stores.chat, attempt);
    const reply: Promise<Reply> = (async () => {
      try {
        const result = await api.sendChatMessage(chatDraftRequest(attempt));
        validateChatResult(attempt, result);
        return { ok: true as const, result };
      } catch (error) {
        return { ok: false as const, error };
      }
    })();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const deadline = new Promise<"deadline">((resolve) => {
      timer = setTimeout(() => resolve("deadline"), CHAT_SEND_DEADLINE_MS);
    });
    const first = await Promise.race([reply, deadline]);
    clearTimeout(timer);
    if (first === "deadline") {
      // Lean `timeout`: the saved deadline has passed, so every window now
      // shows the message failed. A later reply is still applied.
      stores.chat.setState(loadChatDrafts());
      void reply.then((late) => settle(attempt, late, applied)).catch(() => {});
      throw new Error(
        `No reply arrived within ${CHAT_SEND_DEADLINE_MS / 1000} seconds. Retry sends it again; it is never sent twice.`,
      );
    }
    return settle(attempt, first, applied);
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
            deliveryAffordances(sendStatus(draft, now()), mailbox !== undefined)
              .blocksComposer,
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
      return send(draft, applied);
    },
    /** Resend a failed message: the same payload under the same identity. */
    async retry(requestId: string, applied: Applied) {
      const draft = original(requestId);
      const status = sendStatus(draft, now());
      if (status === "pending") throw new Error("This message is still being sent.");
      if (status === "confirmed")
        throw new Error("This message was already delivered.");
      return send(draft, applied);
    },
    /** Discard a failed message. It sends nothing; if an earlier send of it
     * reached the runtime, it still appears in the session. */
    async discard(requestId: string) {
      const draft = original(requestId);
      if (sendStatus(draft, now()) !== "failed")
        throw new Error("Only a message that wasn't sent can be discarded.");
      removeChatDraft(stores.chat, draft);
    },
  };
}
