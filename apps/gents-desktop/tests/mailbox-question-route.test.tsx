import { acceptedChatReply } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  MailboxItemView,
} from "@source-inc/gents-desktop-client";
import { createMailboxActions } from "../src/hooks/mailboxActions";
import { loadChatDrafts } from "../src/hooks/chatDrafts";
import { shellStores } from "./shell-fixture";

const item: MailboxItemView = {
  itemId: "item-1",
  itemKey: "item-1",
  requesterDid: "test-client",
  nodeDid: "did:source-node",
  status: "open",
  action: "start_request",
  kind: "ask",
  title: "Question",
  summary: null,
  payload: null,
  sourceKind: "session",
  sourceId: "session-1",
  requestId: null,
  causeDocId: null,
  targetNodeDid: "did:node",
  targetAgentDid: "engineer",
  sessionId: "session-1",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: "2026-10-28T00:00:00Z",
};

function mailbox(api: DesktopApiAdapter) {
  const stores = shellStores({ deployments: [] });
  const actions = createMailboxActions({
    api,
    stores,
    refreshSnapshot: async () => {},
    reportFailure: () => {},
  });
  return { store: stores.selection, actions };
}

describe("answering a mailbox question", () => {
  it("sends the answer as the item's reply and retires a compose route on it", async () => {
    const sendChatMessage = vi.fn(acceptedChatReply);
    const api = {
      startMailboxRequest: vi.fn().mockResolvedValue(item),
      sendChatMessage,
    } as unknown as DesktopApiAdapter;
    const { store, actions } = mailbox(api);
    await actions.openMailboxItem(item.itemId);
    expect(store.getState().mailboxRoute?.itemId).toBe("item-1");
    const answer = { option_ids: ["yes"], free_text: null };
    await actions.answerMailboxQuestion(item, answer);
    expect(sendChatMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        nodeDid: "did:node",
        agentDid: "engineer",
        sessionId: "session-1",
        content: "",
        causedBySourceDocId: "item-1",
        answer,
      }),
    );
    expect(loadChatDrafts().chatDrafts[0]?.mailbox).toEqual({
      nodeDid: "did:source-node",
      itemId: "item-1",
      summary: "yes",
    });
    expect(store.getState().mailboxRoute).toBeNull();
  });
});

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());
