import { describe, expect, it } from "vitest";

import { createDesktopClient } from "./client.js";
import { createMemoryTransport } from "./testing.js";

describe("mailbox client API", () => {
  it("uses first-class list/start/dismiss commands and carries submit correlation", async () => {
    const row = { itemId: "item-1", action: "start_request" };
    const transport = createMemoryTransport({
      handlers: {
        desktop_mailbox_list: () => [row],
        desktop_mailbox_start_request: () => row,
        desktop_mailbox_dismiss: () => undefined,
        desktop_chat_send: () => ({ sessionId: "s", requestId: "r" }),
      },
    });
    const api = createDesktopClient(transport).api;

    await expect(api.listMailbox()).resolves.toEqual([row]);
    await expect(api.startMailboxRequest("item-1")).resolves.toEqual(row);
    await expect(api.dismissMailboxItem("item-1")).resolves.toBeUndefined();
    await api.sendChatMessage({
      agentDid: "did:agent",
      behaviorId: "operator",
      content: "continue",
      causedBySourceDocId: "item-1",
    });

    expect(transport.calls).toEqual([
      { command: "desktop_mailbox_list", args: undefined },
      {
        command: "desktop_mailbox_start_request",
        args: { request: { itemId: "item-1" } },
      },
      {
        command: "desktop_mailbox_dismiss",
        args: { request: { itemId: "item-1" } },
      },
      {
        command: "desktop_chat_send",
        args: {
          request: {
            agentDid: "did:agent",
            behaviorId: "operator",
            content: "continue",
            causedBySourceDocId: "item-1",
          },
        },
      },
    ]);
  });
});
