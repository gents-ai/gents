import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  MailboxItemView,
} from "@source-inc/gents-desktop-client";
import { createDesktopShellMailboxActions } from "../src/hooks/desktopShellMailboxActions";
import { createSelectionStore } from "../src/hooks/selectionStore";

const item = {
  itemId: "item-1",
  action: "start_request",
  kind: "ask",
  targetAgentDid: "did:agent",
  targetBehaviorId: "engineer",
  sessionId: "session-1",
} as MailboxItemView;

function mailbox(api: DesktopApiAdapter) {
  const store = createSelectionStore();
  const actions = createDesktopShellMailboxActions({
    api,
    store,
    refreshSnapshot: async () => {},
    setError: () => {},
    setSession: () => {},
  });
  return { store, actions };
}

describe("answering a mailbox question", () => {
  it("sends the answer as the item's reply and retires a compose route on it", async () => {
    const sendChatMessage = vi.fn().mockResolvedValue({});
    const api = {
      startMailboxRequest: vi.fn().mockResolvedValue(item),
      sendChatMessage,
    } as unknown as DesktopApiAdapter;
    const { store, actions } = mailbox(api);
    await actions.onOpenMailboxItem(item.itemId);
    expect(store.getState().mailboxRoute?.itemId).toBe("item-1");
    const answer = { option_ids: ["yes"], free_text: null };
    await actions.onAnswerMailboxQuestion(item, answer);
    expect(sendChatMessage).toHaveBeenCalledWith({
      agentDid: "did:agent",
      behaviorId: "engineer",
      sessionId: "session-1",
      content: "",
      causedBySourceDocId: "item-1",
      answer,
    });
    expect(store.getState().mailboxRoute).toBeNull();
  });
});
