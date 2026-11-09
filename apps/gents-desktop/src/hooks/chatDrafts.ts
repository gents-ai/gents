import type {
  BridgeErrorPayload,
  ChatSendRequest,
  ChatSendResult,
} from "@source-inc/gents-desktop-client";
import type { ChatStore } from "./chatStore";

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
   * `ClientOptimisticSend.payload_retained`); a retry resends it as is. */
  requestJson: string;
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
    const released: string[] = [];
    for (let index = 0; index < localStorage.length; index++) {
      const key = localStorage.key(index);
      if (!key?.startsWith(PREFIX)) continue;
      const draft = decode(JSON.parse(localStorage.getItem(key) ?? "null"));
      if (chatDraftKey(draft) !== key)
        throw new Error("The saved message identity is inconsistent.");
      // A confirmed chat message keeps no payload (Lean `onReply`); only a
      // confirmed answer waits for its question to close.
      if (draft.confirmed && !draft.mailbox) released.push(key);
      else chatDrafts.push(draft);
    }
    for (const key of released) localStorage.removeItem(key);
    return { chatDrafts, chatDraftError: null };
  } catch (error) {
    return {
      chatDrafts: [],
      chatDraftError:
        error instanceof Error ? error.message : "Saved messages could not be read.",
    };
  }
}

export function retainChatDraft(
  request: ChatSendRequest,
  selectedSessionId: string | null,
  queued = false,
  mailbox?: ChatDraft["mailbox"],
): ChatDraft {
  return decode({
    ...request,
    selectedSessionId,
    queued,
    mailbox,
    requestJson: JSON.stringify(request),
  });
}

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

/** Each message has its own key, so another window cannot overwrite it. The
 * payload is kept before dispatch: a storage refusal prevents the send. */
export function saveChatDraft(store: ChatStore, draft: ChatDraft) {
  const key = chatDraftKey(draft);
  const previous = localStorage.getItem(key);
  if (
    previous !== null &&
    decode(JSON.parse(previous)).requestJson !== draft.requestJson
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
