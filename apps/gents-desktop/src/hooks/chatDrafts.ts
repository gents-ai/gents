import type {
  AttachmentView,
  BridgeErrorPayload,
  ChatSendRequest,
  ChatSendResult,
} from "@source-inc/gents-desktop-client";
import { attachmentMetadata } from "./attachmentMetadata";
import type { ChatStore } from "./chatStore";
import { sendAttachments } from "./sendAttachments";

const PREFIX = "gents.chat.sendDraft.v1:";
/** The base client's list of sends with an unknown outcome and no kept
 * payload. Nothing reads it: those sends are visible in their sessions if
 * they landed, and no payload remains to resend. */
const ABANDONED_UNRESOLVED = "gents.chat.unresolvedSubmissions.v1";

export type ChatDraft = {
  clientDid: string;
  nodeDid: string;
  requestId: string;
  sessionId: string;
  selectedSessionId: string | null;
  queued: boolean;
  mailbox?: { nodeDid: string; itemId: string; summary: string };
  /** The authored payload, kept until the runtime confirms it (Lean
   * `ClientOptimisticSend.payload_retained`); a retry resends it as is. The
   * attachment bytes are kept beside it in `sendAttachments` under the
   * draft's key, so this holds the request without them. */
  requestJson: string;
  /** The kept attachments, in order; a retry sends only bytes that match. */
  attachments?: AttachmentView[];
  /** The runtime holds this request. Only a mailbox answer stays saved once
   * confirmed, until its question is observed closed. A draft saved by the
   * base client with its receipt (`result`) was confirmed too. */
  confirmed?: true;
  /** The deadline (epoch ms) of the send in flight (Lean `timeout`,
   * `CHAT_SEND_DEADLINE_MS`). It is saved with the payload, so every window
   * derives the same status: pending until it passes, failed after. */
  pendingUntil?: number;
  /** Why the last send failed, shown with Retry: the cause and, when the
   * bridge classified it, its typed error (storage full names a recovery). */
  failure?: { message: string; error: BridgeErrorPayload | null };
};

const identifier = (value: unknown): value is string =>
  typeof value === "string" && value.trim().length > 0;
const object = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const attachmentView = (value: unknown): value is AttachmentView =>
  object(value) &&
  typeof value.name === "string" &&
  typeof value.mediaType === "string" &&
  typeof value.digest === "string" &&
  typeof value.size === "number";

/** Drops the kept attachment bytes of a send that is released, discarded or
 * never saved. A failed delete leaves unreferenced bytes, never a send
 * without them. */
export function forgetChatDraftAttachments(draft: ChatDraft) {
  if (draft.attachments?.length)
    void sendAttachments()
      .delete(chatDraftKey(draft))
      .catch(() => {});
}

export function chatDraftKey(
  draft: Pick<ChatDraft, "clientDid" | "nodeDid" | "requestId">,
) {
  return PREFIX + JSON.stringify([draft.clientDid, draft.nodeDid, draft.requestId]);
}

function decode(value: unknown): ChatDraft {
  if (
    !object(value) ||
    !identifier(value.clientDid) ||
    !identifier(value.nodeDid) ||
    !identifier(value.requestId) ||
    !identifier(value.sessionId) ||
    !(value.selectedSessionId === null || identifier(value.selectedSessionId)) ||
    typeof value.requestJson !== "string"
  )
    throw new Error(
      "A saved message could not be read. Check this history before sending again.",
    );
  const draft: ChatDraft = {
    clientDid: value.clientDid,
    nodeDid: value.nodeDid,
    requestId: value.requestId,
    sessionId: value.sessionId,
    selectedSessionId: value.selectedSessionId,
    queued: value.queued === true,
    requestJson: value.requestJson,
  };
  if (value.mailbox !== undefined) {
    if (
      !object(value.mailbox) ||
      !identifier(value.mailbox.itemId) ||
      !identifier(value.mailbox.nodeDid) ||
      typeof value.mailbox.summary !== "string"
    )
      throw new Error("The saved answer's question could not be read.");
    draft.mailbox = {
      nodeDid: value.mailbox.nodeDid,
      itemId: value.mailbox.itemId,
      summary: value.mailbox.summary,
    };
  }
  if (value.attachments !== undefined) {
    if (!Array.isArray(value.attachments) || !value.attachments.every(attachmentView))
      throw new Error("The saved message's attachments could not be read.");
    draft.attachments = value.attachments.map(({ name, mediaType, digest, size }) => ({
      name,
      mediaType,
      digest,
      size,
    }));
  }
  if (value.confirmed === true || object(value.result)) draft.confirmed = true;
  else if (
    typeof value.pendingUntil === "number" &&
    Number.isFinite(value.pendingUntil)
  )
    draft.pendingUntil = value.pendingUntil;
  if (object(value.failure) && typeof value.failure.message === "string")
    draft.failure = {
      message: value.failure.message,
      error: object(value.failure.error)
        ? (value.failure.error as BridgeErrorPayload)
        : null,
    };
  return draft;
}

export function loadChatDrafts(): {
  chatDrafts: ChatDraft[];
  chatDraftError: string | null;
} {
  try {
    localStorage.removeItem(ABANDONED_UNRESOLVED);
    const chatDrafts: ChatDraft[] = [];
    const released: ChatDraft[] = [];
    for (let index = 0; index < localStorage.length; index++) {
      const key = localStorage.key(index);
      if (!key?.startsWith(PREFIX)) continue;
      const draft = decode(JSON.parse(localStorage.getItem(key) ?? "null"));
      if (chatDraftKey(draft) !== key)
        throw new Error("The saved message identity is inconsistent.");
      // A confirmed chat message keeps no payload (Lean `onReply`); only a
      // confirmed answer waits for its question to close.
      if (draft.confirmed && !draft.mailbox) released.push(draft);
      else chatDrafts.push(draft);
    }
    for (const draft of released) {
      localStorage.removeItem(chatDraftKey(draft));
      forgetChatDraftAttachments(draft);
    }
    return { chatDrafts, chatDraftError: null };
  } catch (error) {
    return {
      chatDrafts: [],
      chatDraftError:
        error instanceof Error ? error.message : "Saved messages could not be read.",
    };
  }
}

/** The draft of a request without attachments. A request with attachments
 * is kept through `retainChatDraft`, which keeps their bytes first. */
export function chatDraftOf(
  request: ChatSendRequest,
  selectedSessionId: string | null,
  queued = false,
  mailbox?: ChatDraft["mailbox"],
  attachments?: AttachmentView[],
): ChatDraft {
  const { attachments: files, ...input } = request;
  if (files?.length && files.length !== attachments?.length)
    throw new Error("The message's attachments were not kept.");
  return decode({
    ...input,
    selectedSessionId,
    queued,
    mailbox,
    requestJson: JSON.stringify(input),
    attachments: attachments?.length ? attachments : undefined,
  });
}

/** The draft of a new send. Its attachment bytes are kept before it returns,
 * so a send whose bytes cannot be kept is refused before anything is saved
 * or dispatched; the caller saves the draft or abandons it. */
export async function retainChatDraft(
  request: ChatSendRequest,
  selectedSessionId: string | null,
  queued = false,
  mailbox?: ChatDraft["mailbox"],
): Promise<ChatDraft> {
  const files = request.attachments ?? [];
  const draft = chatDraftOf(
    request,
    selectedSessionId,
    queued,
    mailbox,
    await Promise.all(files.map(attachmentMetadata)),
  );
  if (files.length)
    try {
      await sendAttachments().put(
        chatDraftKey(draft),
        files.map((file) => file.data),
      );
    } catch (error) {
      throw new Error(
        `The attachments could not be saved on this device, so the message was not sent: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  return draft;
}

/** The saved input without its attachment bytes. */
export function chatDraftRequest(draft: ChatDraft): ChatSendRequest {
  const request: unknown = JSON.parse(draft.requestJson);
  if (
    !object(request) ||
    request.clientDid !== draft.clientDid ||
    request.nodeDid !== draft.nodeDid ||
    request.requestId !== draft.requestId ||
    request.sessionId !== draft.sessionId ||
    typeof request.content !== "string" ||
    (draft.mailbox !== undefined &&
      request.causedBySourceDocId !== draft.mailbox.itemId)
  )
    throw new Error("The saved message input is unreadable or has changed identity.");
  return request as ChatSendRequest;
}

/** The whole payload a send or retry dispatches: the saved input with the
 * kept attachment bytes, which must still match what was sent first. */
export async function chatDraftPayload(draft: ChatDraft): Promise<ChatSendRequest> {
  const request = chatDraftRequest(draft);
  if (!draft.attachments?.length) return request;
  const kept = draft.attachments;
  let data: string[] | undefined;
  try {
    data = await sendAttachments().get(chatDraftKey(draft));
  } catch (error) {
    throw new Error(
      `This message's attachments could not be read on this device, so it was not sent: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
  const attachments =
    data?.length === kept.length
      ? kept.map(({ name, mediaType }, index) => ({
          name,
          mediaType,
          data: data[index],
        }))
      : null;
  if (
    !attachments ||
    (await Promise.all(attachments.map(attachmentMetadata))).some(
      (view, index) => view.digest !== kept[index].digest,
    )
  )
    throw new Error(
      "This message's attachments are no longer saved on this device, so it was not sent. Discard it and attach the files again.",
    );
  return { ...request, attachments };
}

/** Each message has its own key, so another window cannot overwrite it. The
 * payload is kept before dispatch: a storage refusal prevents the send. */
export function saveChatDraft(store: ChatStore, draft: ChatDraft) {
  const key = chatDraftKey(draft);
  const previous = localStorage.getItem(key);
  const original = previous === null ? null : decode(JSON.parse(previous));
  if (
    original &&
    (original.requestJson !== draft.requestJson ||
      JSON.stringify(original.attachments) !== JSON.stringify(draft.attachments))
  )
    throw new Error("The original message input cannot be replaced.");
  localStorage.setItem(key, JSON.stringify(draft));
  const loaded = loadChatDrafts();
  if (loaded.chatDraftError) throw new Error(loaded.chatDraftError);
  if (!loaded.chatDrafts.some((row) => chatDraftKey(row) === key))
    throw new Error("The message could not be saved. No new message was submitted.");
  store.setState(loaded);
}

export function removeChatDraft(store: ChatStore, draft: ChatDraft) {
  localStorage.removeItem(chatDraftKey(draft));
  forgetChatDraftAttachments(draft);
  store.setState(loadChatDrafts());
}

/** The saved form of `draft` as every window sees it now, or null once it is
 * discarded or released. */
export function savedChatDraft(draft: ChatDraft): ChatDraft | null {
  const value = localStorage.getItem(chatDraftKey(draft));
  if (value === null) return null;
  const saved = decode(JSON.parse(value));
  return saved.confirmed && !saved.mailbox ? null : saved;
}

export function chatDraftsFor(
  drafts: readonly ChatDraft[],
  clientDid: string | null,
  nodeDid: string | null,
  sessionId: string | null,
  mailboxItemId?: string | null,
) {
  return drafts.filter(
    (draft) =>
      draft.clientDid === clientDid &&
      draft.nodeDid === nodeDid &&
      (mailboxItemId === null ||
        (mailboxItemId ? draft.mailbox?.itemId === mailboxItemId : !draft.mailbox)) &&
      (draft.selectedSessionId === sessionId || draft.sessionId === sessionId),
  );
}

/** A reply belongs to its message: the runtime holds the identity it was sent under. */
export function validateChatResult(draft: ChatDraft, result: ChatSendResult) {
  if (
    result.nodeDid !== draft.nodeDid ||
    result.requestId !== draft.requestId ||
    result.sessionId !== draft.sessionId
  )
    throw new Error("The reply does not match the saved message.");
  let request: ChatSendRequest | null = null;
  try {
    request = chatDraftRequest(draft);
  } catch {}
  if (request?.agentDid && request.agentDid !== result.agentDid)
    throw new Error("The message reply changed its original Agent.");
}
