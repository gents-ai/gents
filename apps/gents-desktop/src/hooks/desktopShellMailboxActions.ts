import type {
  DesktopApiAdapter,
  MailboxItemView,
  MailboxQuestionAnswer,
} from "@source-inc/gents-desktop-client";
import {
  actionFailure,
  dismissMailboxItemAndClearMatchingRoute,
  shownFailure,
} from "./desktopShellRuntime";
import { selection } from "./selectionStore";
import { writeSession } from "./sessionStore";
import type { ShellStores } from "./shellProjection";

type MailboxActionParams = {
  api: DesktopApiAdapter;
  stores: ShellStores;
  refreshSnapshot: () => Promise<void>;
  /** shows a failed action to the person, once */
  reportFailure: (message: string) => void;
};

/** Opening, dismissing and answering mailbox items. An opened item routes
    the next message to it while the selection is the one it set up. */
export function createDesktopShellMailboxActions({
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
        agentDid: item.targetAgentDid,
        behaviorId: item.targetBehaviorId,
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
      await dismissMailboxItemAndClearMatchingRoute(
        itemId,
        (dismissedItemId) => api.dismissMailboxItem(dismissedItemId),
        () => store.getState().mailboxRoute?.itemId ?? null,
        () => selection.releaseMailboxRoute(store),
      );
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
      await api.sendChatMessage({
        agentDid: item.targetAgentDid,
        behaviorId: item.targetBehaviorId,
        sessionId: item.sessionId ?? null,
        content: "",
        causedBySourceDocId: item.itemId,
        answer,
      });
      /* the reply consumed the item, so a compose route opened on it must
         not carry it as the next message's source */
      if (store.getState().mailboxRoute?.itemId === item.itemId) {
        selection.releaseMailboxRoute(store);
      }
      await refreshSnapshot();
    } catch (error) {
      reportFailure(actionFailure("send the answer", error));
      throw shownFailure(error);
    }
  }

  return {
    /**
     * Starts the item's reply: selects the node, behavior and session it names
     * and holds the item as the next message's cause. A navigation: if the
     * person moves on before the bridge answers, the result is dropped and
     * null returned. A failure is reported once, then rethrown.
     */
    openMailboxItem,
    /**
     * Dismisses the item, lets go of it if the next message was going to
     * answer it, then reads the client again. A failure is reported once, then
     * rethrown.
     */
    dismissMailboxItem,
    /**
     * Answers a question item. The answer is the item's ordinary reply
     * request, whose content the bridge renders from the question, so the
     * runtime's reply consumes the item; a compose route opened on it is let
     * go. A failure is reported once, then rethrown.
     */
    answerMailboxQuestion,
  };
}
