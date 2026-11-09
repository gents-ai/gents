import type { ChatDraft } from "./chatDrafts";

/** What this client knows about one saved send (Lean `ClientSubmissionReceipt.Saved`). */
export type SavedDelivery =
  | "sending"
  | "unsent"
  | "delivered"
  | "notDelivered"
  | "unknownRetained"
  | "unknownUnretained";

export type DeliveryView =
  "sending" | "delivered" | "notDelivered" | "awaiting" | "unprovable";

export type DeliveryAffordances = {
  view: DeliveryView;
  check: boolean;
  resume: boolean;
  dismiss: boolean;
  setAside: boolean;
  blocksComposer: boolean;
};

export function savedDelivery(draft: ChatDraft, inFlight: boolean): SavedDelivery {
  if (!draft.result) return inFlight ? "sending" : "unsent";
  switch (draft.result.outcome) {
    case "notDelivered":
      return "notDelivered";
    case "deliveryUnknown":
      return draft.unretained ? "unknownUnretained" : "unknownRetained";
    default:
      return "delivered";
  }
}

/** The view and actions for a saved send, bound to Lean `affordances`. */
export function deliveryAffordances(
  saved: SavedDelivery,
  mailbox: boolean,
): DeliveryAffordances {
  const shown = (
    view: DeliveryView,
    check: boolean,
    resume: boolean,
    dismiss: boolean,
    setAside: boolean,
    blocksComposer: boolean,
  ) => ({ view, check, resume, dismiss, setAside, blocksComposer });
  switch (saved) {
    case "sending":
      return shown("sending", false, false, false, false, true);
    case "unsent":
    case "unknownRetained":
      return shown("awaiting", true, true, false, false, true);
    case "delivered":
      return shown("delivered", true, false, false, false, mailbox);
    case "notDelivered":
      return shown("notDelivered", true, false, true, false, false);
    case "unknownUnretained":
      return shown("unprovable", true, false, false, true, true);
  }
}

/** Only a proven non-delivery authorizes a replacement send. */
export const mayResendNewIdentity = (view: DeliveryView) => view === "notDelivered";
