import type { ChatDraft } from "./chatDrafts";

/** One authored message's send state (Lean `ClientOptimisticSend.Status`).
 * A kept draft is pending until its saved send deadline, confirmed once the
 * runtime holds it, and failed otherwise: a refused send, or one whose reply
 * did not arrive by its deadline (a lost reply, a hung send, a restart, a
 * closed window). Every window derives it from the shared saved draft. */
export type SendStatus = "pending" | "confirmed" | "failed" | "discarded";

export type DeliveryView = "sending" | "delivered" | "failed" | "hidden";

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

export function resendRefused(draft: ChatDraft): boolean {
  return draft.failure?.error?.code === "permissionDenied";
}

/** The view and actions for one message, bound to Lean `view`. */
export function deliveryAffordances(
  status: SendStatus,
  mailbox: boolean,
  refused: boolean,
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
      return {
        view: "failed",
        retry: !refused,
        discard: true,
        blocksComposer: mailbox,
      };
    case "discarded":
      return { view: "hidden", retry: false, discard: false, blocksComposer: false };
  }
}

export function keptRow(
  status: SendStatus,
  mailbox: boolean,
  observed: boolean,
): KeptRow | null {
  if (mailbox || observed) return null;
  return status === "pending" ? "sending" : status === "failed" ? "failed" : null;
}

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
