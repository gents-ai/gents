import type { ChatDraft } from "./chatDrafts";

/** One authored message's send state (Lean `ClientOptimisticSend.Status`).
 * A kept draft is pending while its send is in flight, confirmed once the
 * runtime holds it, and failed otherwise: a refused send, or one whose reply
 * never arrived (a lost reply, a restart). */
export type SendStatus = "pending" | "confirmed" | "failed" | "discarded";

export type DeliveryView = "sending" | "delivered" | "failed" | "hidden";

export type DeliveryAffordances = {
  view: DeliveryView;
  retry: boolean;
  discard: boolean;
  blocksComposer: boolean;
};

export function sendStatus(draft: ChatDraft, inFlight: boolean): SendStatus {
  if (inFlight) return "pending";
  return draft.confirmed ? "confirmed" : "failed";
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
