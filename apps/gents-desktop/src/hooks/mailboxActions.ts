import type {
  DesktopApiAdapter,
  MailboxItemView,
  MailboxQuestionAnswer,
} from "@source-inc/gents-desktop-client";
import { actionFailure, shownFailure } from "./actionFailure";
import { selection } from "./selectionStore";
import { writeSession } from "./sessionStore";
import type { ShellStores } from "./shellProjection";

type MailboxActionParams = {
  api: DesktopApiAdapter;
  stores: ShellStores;
  refreshSnapshot: () => Promise<void>;
  reportFailure: (message: string) => void;
};

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
      const sent = await api.sendChatMessage({
        nodeDid: item.targetNodeDid,
        agentId: item.targetAgentId,
        sessionId: item.sessionId ?? null,
        content: "",
        causedBySourceDocId: item.itemId,
        answer,
      });
      if (sent.outcome === "deliveryUnknown")
        reportFailure(
          "Your answer may have been delivered. Check the session before answering again.",
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
