import type { ChatDraft } from "./chatDrafts";

/** One authored message's send state (Lean `ClientOptimisticSend.Status`).
 * A kept draft is pending until its saved send deadline, confirmed once the
 * runtime holds it, and failed otherwise: a refused send, or one whose reply
 * did not arrive by its deadline (a lost reply, a hung send, a restart, a
 * closed window). Every window derives it from the shared saved draft. */
export type SendStatus = "pending" | "confirmed" | "failed" | "discarded";

export type DeliveryView = "sending" | "delivered" | "failed" | "hidden";

/** What a kept message draws as its own transcript row (Lean `keptRow`). */
export type KeptRow = Extract<DeliveryView, "sending" | "failed">;

export type DeliveryAffordances = {
  view: DeliveryView;
  retry: boolean;
  discard: boolean;
  blocksComposer: boolean;
};

export function sendStatus(draft: ChatDraft, now: number): SendStatus {
  if (draft.confirmed) return "confirmed";
  return draft.pendingUntil !== undefined && now < draft.pendingUntil
    ? "pending"
    : "failed";
}

/** The earliest saved deadline still ahead, when a view must re-derive. */
export function nextSendDeadline(
  drafts: readonly ChatDraft[],
  now: number,
): number | null {
  let next: number | null = null;
  for (const draft of drafts)
    if (
      !draft.confirmed &&
      draft.pendingUntil !== undefined &&
      draft.pendingUntil > now &&
      (next === null || draft.pendingUntil < next)
    )
      next = draft.pendingUntil;
  return next;
}

/** The view and actions for one message, bound to Lean `view`. */
export function deliveryAffordances(
  status: SendStatus,
  mailbox: boolean,
): DeliveryAffordances {
  switch (status) {
    case "pending":
      return { view: "sending", retry: false, discard: false, blocksComposer: mailbox };
    case "confirmed":
      return {
        view: "delivered",
        retry: false,
        discard: false,
        blocksComposer: mailbox,
      };
    case "failed":
      return { view: "failed", retry: true, discard: true, blocksComposer: mailbox };
    case "discarded":
      return { view: "hidden", retry: false, discard: false, blocksComposer: false };
  }
}

/** The row a kept message draws in its session's transcript, bound to Lean
 * `keptRow`: sending or failed until the session is observed holding its
 * identity (`sessionHoldsRequest`), whose own row then stands for it. */
export function keptRow(
  status: SendStatus,
  mailbox: boolean,
  observed: boolean,
): KeptRow | null {
  if (mailbox || observed) return null;
  return status === "pending" ? "sending" : status === "failed" ? "failed" : null;
}

/** A kept send that its session's transcript draws instead of the composer's
 * feedback: a chat message sent from that session's own composer. A queued
 * one waits in the pending tray, a new session's first message has no
 * transcript yet, and an answer is shown with its question. */
export function drawnInTranscript(
  draft: ChatDraft,
  nodeDid: string | null,
  sessionId: string | null,
): boolean {
  return (
    !draft.mailbox &&
    !draft.queued &&
    draft.nodeDid === nodeDid &&
    draft.selectedSessionId !== null &&
    draft.selectedSessionId === draft.sessionId &&
    draft.sessionId === sessionId
  );
}

export type TranscriptSend = { draft: ChatDraft; row: KeptRow };

/** The kept sends of `clientDid` that the session's transcript draws as
 * their own rows, oldest first. `storing` is the send this window saved but
 * whose attachment bytes it is still storing: a failed store refuses it and
 * the composer keeps its text, so it is drawn only once they are stored. */
export function transcriptSends(
  drafts: readonly ChatDraft[],
  clientDid: string | null,
  nodeDid: string | null,
  sessionId: string | null,
  now: number,
  observed: (requestId: string) => boolean,
  storing: string | null,
): TranscriptSend[] {
  const sends: TranscriptSend[] = [];
  for (const draft of drafts) {
    if (
      draft.clientDid !== clientDid ||
      draft.requestId === storing ||
      !drawnInTranscript(draft, nodeDid, sessionId)
    )
      continue;
    const row = keptRow(
      sendStatus(draft, now),
      draft.mailbox !== undefined,
      observed(draft.requestId),
    );
    if (row) sends.push({ draft, row });
  }
  return sends.sort(
    (a, b) =>
      (a.draft.composedAt ?? 0) - (b.draft.composedAt ?? 0) ||
      (a.draft.requestId < b.draft.requestId ? -1 : 1),
  );
}
