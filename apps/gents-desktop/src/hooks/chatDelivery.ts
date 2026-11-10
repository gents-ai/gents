import {
  asBridgeErrorPayload,
  type ChatSendRequest,
  type ChatSendResult,
  type DesktopApiAdapter,
} from "@source-inc/gents-desktop-client";
import {
  chatDraftHandBack,
  chatDraftPayload,
  chatDraftsFor,
  forgetChatDraftAttachments,
  loadChatDrafts,
  removeChatDraft,
  saveChatDraft,
  saveNewChatDraft,
  savedChatDraft,
  validateChatResult,
  type ChatDraft,
} from "./chatDrafts";
import { deliveryAffordances, sendStatus } from "./chatDeliveryView";
import type { ChatStore } from "./chatStore";
import type { SendAttachmentStore } from "./sendAttachments";
import type { ShellStores } from "./shellProjection";
import { heldFor, sessionHoldsRequest } from "./sessionStore";

type Applied = (result: ChatSendResult, draft: ChatDraft) => Promise<boolean>;
/** Called once the payload is saved, before dispatch: from then on the saved
 * message holds the authored input. */
type Saved = (draft: ChatDraft) => void;

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

/** `next(value)`, in the calling task when `value` is already known. Only
 * attachment bytes are waited for, so a send without them is saved and
 * dispatched in the task that submitted it. */
function proceed<T, U>(value: T | Promise<T>, next: (value: T) => U) {
  return value instanceof Promise ? value.then(next) : next(value);
}

/** The runtime holds the message (Lean `onReply true`, `onObserve`). A chat
 * message keeps no payload; a mailbox answer stays saved, confirmed, until its
 * question is observed closed. */
function confirm(store: ChatStore, current: ChatDraft) {
  if (current.mailbox)
    saveChatDraft(store, {
      ...current,
      confirmed: true,
      pendingUntil: undefined,
      failure: undefined,
    });
  else removeChatDraft(store, current);
}

/** The optimistic send (Lean `ClientOptimisticSend`): the message gets its
 * request identity and a kept payload before dispatch, is pending until its
 * saved deadline while the one signed create is in flight, and is confirmed by
 * its reply or failed with Retry, which resends the same payload under the
 * same identity. The saved draft is the message's only state, shared by every
 * window. */
export function createChatDelivery(
  api: DesktopApiAdapter,
  stores: ShellStores,
  attachments: SendAttachmentStore,
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
    // confirmed message as failed.
    confirm(stores.chat, current);
    try {
      await applied(reply.result, current);
    } catch (error) {
      throw new ChatViewUpdateError(reply.result, error);
    } finally {
      if (!current.mailbox) await forgetChatDraftAttachments(attachments, current);
    }
    return reply.result;
  }
  /** Saves `draft` as a send in flight until its deadline. */
  function attempt(draft: ChatDraft) {
    if (clientDid() !== draft.clientDid)
      throw new Error("The selected client changed before dispatch.");
    const pending: ChatDraft = {
      ...draft,
      failure: undefined,
      pendingUntil: now() + CHAT_SEND_DEADLINE_MS,
    };
    saveChatDraft(stores.chat, pending);
    return pending;
  }
  /** Dispatches a saved attempt and settles its reply. */
  async function send(attempt: ChatDraft, applied: Applied, saved?: Saved) {
    saved?.(attempt);
    const reply: Promise<Reply> = (async () => {
      try {
        const result = await proceed(
          chatDraftPayload(attachments, attempt),
          (payload) => api.sendChatMessage(payload),
        );
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
      saved?: Saved,
    ) {
      const actor = clientDid();
      if (request.clientDid && request.clientDid !== actor)
        throw new Error("The selected client changed before dispatch.");
      const input: ChatSendRequest = {
        ...request,
        clientDid: actor,
        requestId: crypto.randomUUID(),
        sessionId: request.sessionId ?? crypto.randomUUID(),
      };
      const admitted = (draft: ChatDraft) => {
        if (
          chatDraftsFor(
            load(),
            actor,
            request.nodeDid,
            selectedSessionId,
            mailbox?.itemId,
          ).some(
            (row) =>
              deliveryAffordances(sendStatus(row, now()), mailbox !== undefined)
                .blocksComposer,
          )
        )
          throw new Error(
            "Retry or discard the saved answer before answering this question again.",
          );
        return attempt(draft);
      };
      return proceed(
        saveNewChatDraft(
          attachments,
          stores.chat,
          input,
          selectedSessionId,
          queued,
          mailbox,
          admitted,
        ),
        (draft) => send(draft, applied, saved),
      );
    },
    /** Resend a failed message: the same payload under the same identity. */
    async retry(requestId: string, applied: Applied) {
      const draft = original(requestId);
      const status = sendStatus(draft, now());
      if (status === "pending") throw new Error("This message is still being sent.");
      if (status === "confirmed")
        throw new Error("This message was already delivered.");
      return send(attempt(draft), applied);
    },
    /** Discard a failed message and return what it gives back to its
     * composer. It sends nothing; if an earlier send of it reached the
     * runtime, it still appears in the session. */
    async discard(requestId: string) {
      // Read before the bytes are deleted with the draft.
      const handed = await chatDraftHandBack(attachments, original(requestId));
      const draft = original(requestId);
      if (sendStatus(draft, now()) !== "failed")
        throw new Error("Only a message that wasn't sent can be discarded.");
      removeChatDraft(stores.chat, draft);
      await forgetChatDraftAttachments(attachments, draft);
      return handed;
    },
  };
}

/** Lean `observe`: a failed message that the selected session holds has
 * landed (its reply was lost, came after the deadline, or the app restarted),
 * so it is confirmed instead of offering Retry. A pending message waits for
 * its reply or deadline, so the reply still updates the view that sent it; the
 * sweep runs again when the deadline clock ticks. This is the one place a
 * composer error about a saved send is cleared: once no unconfirmed copy of
 * that send remains (confirmed by its reply or here, or discarded). */
export function observeLandedChatSends(
  stores: Pick<ShellStores, "chat" | "client" | "session">,
  now: () => number = Date.now,
) {
  let disposed = false;
  let scheduled = false;
  function schedule() {
    if (disposed || scheduled) return;
    scheduled = true;
    queueMicrotask(() => {
      scheduled = false;
      if (!disposed) sweep();
    });
  }
  function sweep() {
    const held = stores.session.getState();
    const session = held.session;
    const clientDid = stores.client.getState().snapshot?.client?.clientDid;
    const at = now();
    if (session && clientDid)
      for (const draft of stores.chat.getState().chatDrafts) {
        if (
          draft.clientDid !== clientDid ||
          sendStatus(draft, at) !== "failed" ||
          !heldFor(session, draft.sessionId, draft.nodeDid) ||
          !sessionHoldsRequest(held, draft.requestId)
        )
          continue;
        try {
          const current = savedChatDraft(draft);
          if (current && sendStatus(current, at) === "failed")
            confirm(stores.chat, current);
        } catch {
          // An unreadable or unwritable draft keeps showing Retry and Discard.
        }
      }
    const { submissionError, chatDrafts } = stores.chat.getState();
    const about = submissionError?.requestId;
    if (
      about &&
      !chatDrafts.some((draft) => draft.requestId === about && !draft.confirmed)
    )
      stores.chat.setState({ submissionError: null });
  }
  const stop = [
    stores.session.subscribe(schedule),
    stores.client.subscribe((state, previous) => {
      if (state.snapshot?.client?.clientDid !== previous.snapshot?.client?.clientDid)
        schedule();
    }),
    stores.chat.subscribe((state, previous) => {
      if (
        state.chatDrafts !== previous.chatDrafts ||
        state.submissionError !== previous.submissionError ||
        state.sendDeadlineTick !== previous.sendDeadlineTick
      )
        schedule();
    }),
  ];
  schedule();
  return () => {
    disposed = true;
    stop.forEach((unsubscribe) => unsubscribe());
  };
}
