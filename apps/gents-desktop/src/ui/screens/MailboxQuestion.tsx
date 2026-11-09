import { parseMailboxQuestion } from "@source-inc/gents-desktop-client";
export const parseQuestion = parseMailboxQuestion;
import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { useStore } from "zustand";
import { useApp } from "@/app/AppContext";
import { chatDraftsFor } from "../../hooks/chatDrafts";

/** Where this client's answer to `m` is: saved and not yet confirmed (sending,
    or failed with Retry), or confirmed and waiting for the question to close,
    and whether one is still being sent. */
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
    pending: drafts.some((draft) => !draft.confirmed),
    confirmed: drafts.some((draft) => draft.confirmed === true),
    /* the answer is still on its way, so whether it arrived is not yet known */
    inFlight: drafts.some((draft) => requests.has(draft.requestId)),
  };
}
