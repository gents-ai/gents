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
