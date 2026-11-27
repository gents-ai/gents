import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  ChatSendResult,
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { createChatActions } from "../src/hooks/chatActions";
import {
  CHAT_SEND_DEADLINE_MS,
  createChatDelivery,
  observeLandedChatSends,
} from "../src/hooks/chatDelivery";
import { createChatStore } from "../src/hooks/chatStore";
import { loadChatDrafts } from "../src/hooks/chatDrafts";
import { sendStatus } from "../src/hooks/chatDeliveryView";
import { admittingProjection, shellStores } from "./shell-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { acceptedChat } from "./chat-delivery-fixture";
import { deferred } from "./deferred";
import { memorySendAttachments } from "./send-attachments-fixture";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function fixture(sessionId: string | null = "session") {
  const stores = shellStores({
    deployments: [{ nodeDid: "node", sessions: [], mailboxItems: [] }],
    selection: { nodeDid: "node", sessionId },
  });
  stores.client.setState({
    snapshot: { client: { clientDid: "client" } } as DesktopClientSnapshot,
  });
  const send = vi
    .fn<(request: ChatSendRequest) => Promise<ChatSendResult>>()
    .mockRejectedValue(new Error("Outer reply was lost"));
  const report = vi.fn();
  const projection = admittingProjection("agent");
  const sendAttachments = memorySendAttachments();
  const actions = () =>
    createChatActions({
      api: { sendChatMessage: send } as unknown as DesktopApiAdapter,
      stores,
      project: () => projection,
      refreshSession: vi.fn(),
      refreshSnapshot: vi.fn(),
      reportFailure: report,
      sendAttachments,
    });
  return { stores, send, report, projection, actions };
}

it("keeps a confirmed message confirmed when the chat view cannot show it", async () => {
  const f = fixture();
  f.send.mockImplementationOnce(async (original) => acceptedChat(original));
  const delivery = createChatDelivery(
    { sendChatMessage: f.send } as unknown as DesktopApiAdapter,
    f.stores,
    memorySendAttachments(),
  );
  const shown = delivery.submit(
    { clientDid: "client", nodeDid: "node", sessionId: "session", content: "Original" },
    "session",
    false,
    async () => {
      throw new Error("attachment preview failed");
    },
  );
  await expect(shown).rejects.toThrow(/was sent, but this chat could not show it/);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
});

it("shows every window the same status and lets no window discard another window's send in flight", async () => {
  vi.useFakeTimers();
  const f = fixture();
  const hung = deferred<ChatSendResult>();
  f.send.mockImplementationOnce(() => hung.promise);
  void f.actions().sendMessage("Original message");
  await vi.waitFor(() => expect(f.send).toHaveBeenCalledTimes(1));
  const request = f.send.mock.calls[0][0];
  // Another window shares the saved drafts but none of this window's memory.
  const other = createChatDelivery(
    { sendChatMessage: f.send } as unknown as DesktopApiAdapter,
    { ...f.stores, chat: createChatStore() },
    memorySendAttachments(),
  );
  const [seen] = loadChatDrafts().chatDrafts;
  expect(sendStatus(seen, Date.now())).toBe("pending");
  await expect(other.discard(request.requestId!)).rejects.toThrow(/can be discarded/);
  await expect(other.retry(request.requestId!, async () => true)).rejects.toThrow(
    /still being sent/,
  );
  await vi.advanceTimersByTimeAsync(CHAT_SEND_DEADLINE_MS);
  await other.discard(request.requestId!);
  // The first window's late failure cannot bring a discarded message back.
  hung.reject(new Error("late failure"));
  await vi.advanceTimersByTimeAsync(0);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
});

it("clears the composer error about a hung send when its late confirmation arrives", async () => {
  vi.useFakeTimers();
  const f = fixture();
  const stop = observeLandedChatSends(f.stores);
  const hung = deferred<ChatSendResult>();
  f.send.mockImplementationOnce(() => hung.promise);
  const sent = f.actions().sendMessage("Original message");
  await vi.waitFor(() => expect(f.send).toHaveBeenCalledTimes(1));
  const request = f.send.mock.calls[0][0];
  await vi.advanceTimersByTimeAsync(CHAT_SEND_DEADLINE_MS);
  expect(await sent).toBeNull();
  expect(f.stores.chat.getState().submissionError).toMatchObject({
    requestId: request.requestId,
    message: expect.stringContaining("No reply arrived"),
  });
  hung.resolve(acceptedChat(request));
  await vi.waitFor(() => expect(f.stores.chat.getState().submissionError).toBeNull());
  expect(loadChatDrafts().chatDrafts).toEqual([]);
  stop();
});
