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
  setError: (error: string | null) => void;
};

/** Opening, dismissing and answering mailbox items. An opened item routes
    the next message to it while the selection is the one it set up. */
export function createDesktopShellMailboxActions({
  api,
  stores,
  refreshSnapshot,
  setError,
}: MailboxActionParams) {
  const store = stores.selection;
  async function onOpenMailboxItem(itemId: string): Promise<MailboxItemView | null> {
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
      setError(null);
      return item;
    } catch (error) {
      if (!selection.acceptsIntent(store, captured)) return null;
      setError(actionFailure("open the item", error));
      throw shownFailure(error);
    }
  }

  async function onDismissMailboxItem(itemId: string) {
    try {
      await dismissMailboxItemAndClearMatchingRoute(
        itemId,
        (dismissedItemId) => api.dismissMailboxItem(dismissedItemId),
        () => store.getState().mailboxRoute?.itemId ?? null,
        () => selection.releaseMailboxRoute(store),
      );
      await refreshSnapshot();
    } catch (error) {
      setError(actionFailure("dismiss the item", error));
      throw shownFailure(error);
    }
  }

  /* The answer is the item's ordinary reply request; the bridge renders its
     content from the question so the runtime reply claim consumes the item. */
  async function onAnswerMailboxQuestion(
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
      setError(null);
      await refreshSnapshot();
    } catch (error) {
      setError(actionFailure("send the answer", error));
      throw shownFailure(error);
    }
  }

  return { onOpenMailboxItem, onDismissMailboxItem, onAnswerMailboxQuestion };
}
