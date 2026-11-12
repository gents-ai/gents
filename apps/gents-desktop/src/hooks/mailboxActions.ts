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
  const delivery = createChatDelivery(api, stores);
  async function openMailboxItem(
    itemId: string,
    nodeDid?: string | null,
  ): Promise<MailboxItemView | null> {
    selection.advanceIntent(store);
    const captured = selection.captureIntent(store);
    try {
      const item = await api.startMailboxRequest(itemId, nodeDid);
      if (!selection.acceptsIntent(store, captured)) return null;
      selection.openMailboxRoute(store, {
        itemId: item.itemId,
        nodeDid: item.targetNodeDid,
        agentDid: item.targetAgentDid,
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

  async function dismissMailboxItem(itemId: string, nodeDid?: string | null) {
    const clientDid = stores.client.getState().snapshot?.client?.clientDid;
    const intent = selection.captureIntent(store);
    try {
      await api.dismissMailboxItem(itemId, nodeDid);
      if (stores.client.getState().snapshot?.client?.clientDid !== clientDid) return;
      for (const draft of loadChatDrafts().chatDrafts) {
        if (
          draft.clientDid !== clientDid ||
          draft.mailbox?.itemId !== itemId ||
          !draft.confirmed
        )
          continue;
        removeChatDraft(stores.chat, draft);
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
      const question = parseMailboxQuestion(item);
      const summary = [
        ...answer.option_ids.map(
          (id) => question?.options.find((option) => option.id === id)?.label ?? id,
        ),
        answer.free_text,
      ]
        .filter(Boolean)
        .join("\n");
      await delivery.submit(
        {
          clientDid,
          nodeDid: item.targetNodeDid,
          agentDid: item.targetAgentDid,
          sessionId: item.sessionId,
          content: "",
          causedBySourceDocId: item.itemId,
          answer,
        },
        item.sessionId,
        false,
        (result, draft) => applyAnswer(result, draft, intent, acceptsView),
        { nodeDid: item.nodeDid, itemId: item.itemId, summary },
      );
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

  function savedAnswer(itemId: string, requestId: string) {
    const clientDid = stores.client.getState().snapshot?.client?.clientDid;
    const draft = loadChatDrafts().chatDrafts.find(
      (draft) => draft.clientDid === clientDid && draft.requestId === requestId,
    );
    if (draft?.mailbox?.itemId !== itemId)
      throw new Error("The saved answer belongs to another question or client.");
    return clientDid;
  }

  /** Resend a failed answer under its original identity. */
  async function retryMailboxAnswer(
    itemId: string,
    requestId: string,
    acceptsView: () => boolean = () => true,
  ) {
    const clientDid = savedAnswer(itemId, requestId);
    const intent = selection.captureIntent(store);
    const result = await delivery.retry(requestId, (result, draft) =>
      applyAnswer(result, draft, intent, acceptsView),
    );
    if (
      acceptsView() &&
      stores.client.getState().snapshot?.client?.clientDid === clientDid
    )
      void Promise.resolve()
        .then(refreshSnapshot)
        .catch(() => {});
    return result;
  }

  async function discardMailboxAnswer(itemId: string, requestId: string) {
    savedAnswer(itemId, requestId);
    await delivery.discard(requestId);
  }

  return {
    openMailboxItem,
    dismissMailboxItem,
    answerMailboxQuestion,
    retryMailboxAnswer,
    discardMailboxAnswer,
  };
}
