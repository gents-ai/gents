import type {
  DesktopApiAdapter,
  MailboxItemView,
  MailboxQuestionAnswer,
} from "@source-inc/gents-desktop-client";
import { actionFailure, shownFailure } from "./actionFailure";
import { selection } from "./selectionStore";
import { writeSession } from "./sessionStore";
import type { ShellStores } from "./shellProjection";
import {
  contentKey,
  forgetUnresolvedSubmission,
  rememberUnresolvedSubmission,
  unresolvedSubmissionFor,
} from "./unresolvedSubmissions";

type MailboxActionParams = {
  api: DesktopApiAdapter;
  stores: ShellStores;
  refreshSnapshot: () => Promise<void>;
  reportFailure: (message: string) => void;
};

export const MAILBOX_DELIVERY_UNKNOWN_HINT =
  "Your answer may have been delivered. Check the session before answering again; answering again checks delivery instead of creating a duplicate.";

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
    try {
      await api.dismissMailboxItem(itemId);
      selection.releaseMailboxRoute(store, itemId);
      await refreshSnapshot();
    } catch (error) {
      reportFailure(actionFailure("dismiss the item", error));
      throw shownFailure(error);
    }
  }

  async function answerMailboxQuestion(
    item: MailboxItemView,
    answer: MailboxQuestionAnswer,
  ) {
    try {
      // An answer whose delivery is unknown is reconciled, never re-sent under
      // a fresh identity: the owner may already hold the original request.
      const answerKey = mailboxAnswerKey(item.itemId);
      const earlier = unresolvedSubmissionFor(
        item.targetNodeDid,
        item.sessionId ?? null,
        answerKey,
      );
      const sent = await api.sendChatMessage({
        nodeDid: item.targetNodeDid,
        agentId: item.targetAgentId,
        sessionId: earlier?.sessionId ?? item.sessionId ?? null,
        content: "",
        causedBySourceDocId: item.itemId,
        answer,
        ...(earlier ? { resolveRequestId: earlier.requestId } : {}),
      });
      if (earlier) forgetUnresolvedSubmission(earlier.requestId);
      if (sent?.outcome === "notDelivered") {
        reportFailure("Your earlier answer was not delivered. Answer again.");
        return;
      }
      if (sent?.outcome === "deliveryUnknown") {
        rememberUnresolvedSubmission({
          nodeDid: sent.nodeDid,
          requestId: sent.requestId,
          sessionId: sent.sessionId,
          selectedSessionId: item.sessionId ?? null,
          contentKey: contentKey(answerKey),
        });
        reportFailure(MAILBOX_DELIVERY_UNKNOWN_HINT);
        return;
      }
      if (earlier)
        reportFailure(
          "Your earlier answer was delivered; this retry did not send a different answer.",
        );
      selection.releaseMailboxRoute(store, item.itemId);
      await refreshSnapshot();
    } catch (error) {
      reportFailure(actionFailure("send the answer", error));
      throw shownFailure(error);
    }
  }

  return {
    openMailboxItem,
    dismissMailboxItem,
    answerMailboxQuestion,
  };
}
