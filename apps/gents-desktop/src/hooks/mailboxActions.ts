import type {
  DesktopApiAdapter,
  MailboxItemView,
  MailboxQuestionAnswer,
  ChatSendResult,
} from "@source-inc/gents-desktop-client";
import { parseMailboxQuestion } from "@source-inc/gents-desktop-client";
import { createChatDelivery } from "./chatDelivery";
import { loadChatDrafts, removeChatDraft, type ChatDraft } from "./chatDrafts";
import { actionFailure, shownFailure } from "./actionFailure";
import { selection } from "./selectionStore";
import { writeSession } from "./sessionStore";
import type { ShellStores } from "./shellProjection";
import {
  forgetUnresolvedSubmission,
  unresolvedSubmissionFor,
} from "./unresolvedSubmissions";

type MailboxActionParams = {
  api: DesktopApiAdapter;
  stores: ShellStores;
  refreshSnapshot: () => Promise<void>;
  reportFailure: (message: string) => void;
};

export const MAILBOX_DELIVERY_UNKNOWN_HINT =
  "Your answer may have been delivered. Use Check answer delivery or Resume saved answer to resolve the original answer.";

function mailboxAnswerKey(itemId: string): string {
  return `mailbox-answer:${itemId}`;
}

export function createMailboxActions({
  api,
  stores,
  refreshSnapshot,
  reportFailure,
}: MailboxActionParams) {
  const store = stores.selection;
  const delivery = createChatDelivery(api, stores);
  async function openMailboxItem(itemId: string): Promise<MailboxItemView | null> {
    selection.advanceIntent(store);
    const captured = selection.captureIntent(store);
    try {
      const item = await api.startMailboxRequest(itemId);
      if (!selection.acceptsIntent(store, captured)) return null;
      selection.openMailboxRoute(store, {
        itemId: item.itemId,
        nodeDid: item.targetNodeDid,
        agentId: item.targetAgentId,
        sessionId: item.sessionId ?? null,
      });
      writeSession(stores.session, null);
      return item;
    } catch (error) {
      if (!selection.acceptsIntent(store, captured)) return null;
      reportFailure(actionFailure("open the item", error));
      throw shownFailure(error);
    }
  }

  async function dismissMailboxItem(itemId: string) {
    const clientDid = stores.client.getState().snapshot?.client?.clientDid;
    const intent = selection.captureIntent(store);
    try {
      await api.dismissMailboxItem(itemId);
      if (stores.client.getState().snapshot?.client?.clientDid !== clientDid) return;
      for (const draft of loadChatDrafts().chatDrafts) {
        if (
          draft.clientDid !== clientDid ||
          draft.mailbox?.itemId !== itemId ||
          !draft.result ||
          draft.result.outcome
        )
          continue;
        try {
          await api.acknowledgeChatSubmission(draft.result.submissionReceipt!);
          removeChatDraft(stores.chat, draft);
        } catch {}
      }
      if (selection.acceptsIntent(store, intent))
        selection.releaseMailboxRoute(store, itemId);
      void Promise.resolve()
        .then(refreshSnapshot)
        .catch(() => {});
    } catch (error) {
      reportFailure(actionFailure("dismiss the item", error));
      throw shownFailure(error);
    }
  }

  /** An admitted reply can wait for claim while the question is still open.
   * Keep its confirmed form across reloads, so it is not offered as a new answer. */
  async function applyAnswer(
    _result: ChatSendResult,
    draft: ChatDraft,
    intent: number,
    acceptsView: () => boolean,
  ) {
    if (!draft.mailbox) throw new Error("The saved message is not a question answer.");
    if (
      acceptsView() &&
      stores.client.getState().snapshot?.client?.clientDid === draft.clientDid &&
      selection.acceptsIntent(store, intent)
    )
      selection.releaseMailboxRoute(store, draft.mailbox.itemId);
    return false;
  }

  async function answerMailboxQuestion(
    item: MailboxItemView,
    answer: MailboxQuestionAnswer,
    acceptsView: () => boolean = () => true,
  ): Promise<boolean> {
    const intent = selection.captureIntent(store);
    const clientDid = stores.client.getState().snapshot?.client?.clientDid;
    try {
      if (!clientDid)
        throw new Error("Connect this client before answering a question.");
      if (!item.sessionId)
        throw new Error(
          "This question has no destination chat. Refresh it before answering.",
        );
      const earlier = unresolvedSubmissionFor(
        item.targetNodeDid,
        item.sessionId,
        mailboxAnswerKey(item.itemId),
      );
      if (earlier) {
        const sent = await api.sendChatMessage({
          nodeDid: item.targetNodeDid,
          agentId: item.targetAgentId,
          sessionId: earlier.sessionId,
          content: "",
          causedBySourceDocId: item.itemId,
          answer,
          resolveRequestId: earlier.requestId,
        });
        if (
          sent.nodeDid !== earlier.nodeDid ||
          sent.sessionId !== earlier.sessionId ||
          sent.requestId !== earlier.requestId
        )
          throw new Error("The reply does not match the earlier saved answer.");
        if (sent.outcome !== "deliveryUnknown")
          forgetUnresolvedSubmission(earlier.requestId);
        if (sent.outcome) {
          reportFailure(
            sent.outcome === "notDelivered"
              ? "Your earlier answer was not delivered. Answer again."
              : MAILBOX_DELIVERY_UNKNOWN_HINT,
          );
          return false;
        }
        if (
          acceptsView() &&
          stores.client.getState().snapshot?.client?.clientDid === clientDid &&
          selection.acceptsIntent(store, intent)
        )
          selection.releaseMailboxRoute(store, item.itemId);
        reportFailure(
          "Your earlier answer was delivered; this retry did not send a different answer.",
        );
        void Promise.resolve()
          .then(refreshSnapshot)
          .catch(() => {});
        return true;
      }
      const question = parseMailboxQuestion(item);
      const summary = [
        ...answer.option_ids.map(
          (id) => question?.options.find((option) => option.id === id)?.label ?? id,
        ),
        answer.free_text,
      ]
        .filter(Boolean)
        .join("\n");
      const sent = await delivery.submit(
        {
          clientDid,
          nodeDid: item.targetNodeDid,
          agentId: item.targetAgentId,
          sessionId: item.sessionId,
          content: "",
          causedBySourceDocId: item.itemId,
          answer,
        },
        item.sessionId,
        false,
        (result, draft) => applyAnswer(result, draft, intent, acceptsView),
        { itemId: item.itemId, summary },
      );
      if (sent.outcome) {
        reportFailure(
          sent.outcome === "notDelivered"
            ? "Your answer was not delivered. You can answer again."
            : MAILBOX_DELIVERY_UNKNOWN_HINT,
        );
        return false;
      }
      if (
        acceptsView() &&
        stores.client.getState().snapshot?.client?.clientDid === clientDid
      )
        void Promise.resolve()
          .then(refreshSnapshot)
          .catch(() => {});
      return true;
    } catch (error) {
      if (
        acceptsView() &&
        stores.client.getState().snapshot?.client?.clientDid === clientDid &&
        selection.acceptsIntent(store, intent)
      )
        reportFailure(actionFailure("send the answer", error));
      throw shownFailure(error);
    }
  }

  async function recoverMailboxAnswer(
    itemId: string,
    requestId: string,
    resume = false,
    acceptsView: () => boolean = () => true,
  ) {
    const clientDid = stores.client.getState().snapshot?.client?.clientDid;
    const draft = loadChatDrafts().chatDrafts.find(
      (draft) => draft.clientDid === clientDid && draft.requestId === requestId,
    );
    if (draft?.mailbox?.itemId !== itemId)
      throw new Error("The saved answer belongs to another question or client.");
    const intent = selection.captureIntent(store);
    const result = await delivery.recover(requestId, resume, (result, draft) =>
      applyAnswer(result, draft, intent, acceptsView),
    );
    if (
      result &&
      !result.outcome &&
      acceptsView() &&
      stores.client.getState().snapshot?.client?.clientDid === clientDid
    )
      void Promise.resolve()
        .then(refreshSnapshot)
        .catch(() => {});
    return result;
  }

  return {
    openMailboxItem,
    dismissMailboxItem,
    answerMailboxQuestion,
    recoverMailboxAnswer,
  };
}
