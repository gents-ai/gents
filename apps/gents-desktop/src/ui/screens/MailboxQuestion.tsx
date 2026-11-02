import { parseMailboxQuestion } from "@source-inc/gents-desktop-client";
export const parseQuestion = parseMailboxQuestion;
import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { useStore } from "zustand";
import { useApp } from "@/app/AppContext";
import { chatDraftsFor } from "../../hooks/chatDrafts";

/** Where this client's answer to `m` is: sent and waiting to be confirmed,
    or confirmed, from the answers it keeps until delivery is known, and
    whether one is still being sent. */
export function useAnswerDelivery(m: MailboxItemView) {
  const { stores } = useApp();
  const clientDid = useStore(
    stores.client,
    (state) => state.snapshot?.client?.clientDid ?? null,
  );
  const drafts = chatDraftsFor(
    stores.chat.use.chatDrafts(),
    clientDid,
    m.targetNodeDid,
    m.sessionId,
    m.itemId,
  );
  const requests = stores.chat.use.chatRequestsInFlight();
  return {
    pending: drafts.some(
      (draft) => !draft.result || draft.result.outcome === "deliveryUnknown",
    ),
    confirmed: drafts.some((draft) => draft.result && !draft.result.outcome),
    /* the answer is still on its way, so whether it arrived is not yet known */
    inFlight: drafts.some((draft) => requests.has(draft.requestId)),
  };
}
