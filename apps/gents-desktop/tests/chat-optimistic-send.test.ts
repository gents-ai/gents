import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  ChatSendResult,
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { createChatActions } from "../src/hooks/chatActions";
import { createChatStore } from "../src/hooks/chatStore";
import { loadChatDrafts } from "../src/hooks/chatDrafts";
import { deliveryAffordances, sendStatus } from "../src/hooks/chatDeliveryView";
import { admittingProjection, shellStores } from "./shell-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { acceptedChat } from "./chat-delivery-fixture";
import { deferred } from "./deferred";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());

/* Lean `ClientOptimisticSend`: the client picks the identity, keeps the
   payload, sends once, and a failed message retries the same identity. */
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
  const actions = () =>
    createChatActions({
      api: { sendChatMessage: send } as unknown as DesktopApiAdapter,
      stores,
      project: () => projection,
      refreshSession: vi.fn(),
      refreshSnapshot: vi.fn(),
      reportFailure: report,
    });
  return { stores, send, report, projection, actions };
}

it("keeps the payload under one identity while pending and releases it when confirmed", async () => {
  const f = fixture();
  const reply = deferred<ChatSendResult>();
  f.send.mockImplementationOnce(() => reply.promise);
  const actions = f.actions();
  const sent = actions.sendMessage("Original message");
  await vi.waitFor(() => expect(f.send).toHaveBeenCalledTimes(1));
  const request = f.send.mock.calls[0][0];
  const [draft] = loadChatDrafts().chatDrafts;
  expect(JSON.parse(draft.requestJson)).toEqual(request);
  expect(request).toMatchObject({
    clientDid: "client",
    requestId: draft.requestId,
    sessionId: "session",
    content: "Original message",
  });
  expect(
    sendStatus(
      draft,
      f.stores.chat.getState().chatRequestsInFlight.has(draft.requestId),
    ),
  ).toBe("pending");
  reply.resolve(acceptedChat(request));
  expect((await sent)?.requestId).toBe(request.requestId);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
});

it("shows a failed send with its cause, and Retry resends the same identity and payload", async () => {
  const f = fixture();
  await f
    .actions()
    .sendMessage("Original message", undefined, [
      { name: "note.txt", mediaType: "text/plain", data: "bm90ZQ==" },
    ]);
  const request = f.send.mock.calls[0][0];
  const [failed] = loadChatDrafts().chatDrafts;
  expect(failed.failure?.message).toBe("Outer reply was lost");
  expect(deliveryAffordances(sendStatus(failed, false), false)).toEqual({
    view: "failed",
    retry: true,
    discard: true,
    blocksComposer: false,
  });
  f.send.mockImplementationOnce(async (original) => acceptedChat(original));
  const retried = await f.actions().retryChatMessage(request.requestId!);
  expect(f.send.mock.calls[1][0]).toEqual(request);
  expect(retried.requestId).toBe(request.requestId);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
});

it("restores an unconfirmed message after a restart as failed with Retry", async () => {
  const f = fixture();
  const lost = deferred<ChatSendResult>();
  f.send.mockImplementationOnce(() => lost.promise);
  void f.actions().sendMessage("Original message");
  await vi.waitFor(() => expect(f.send).toHaveBeenCalledTimes(1));
  const request = f.send.mock.calls[0][0];
  f.stores.chat = createChatStore();
  const [restored] = loadChatDrafts().chatDrafts;
  expect(sendStatus(restored, false)).toBe("failed");
  f.send.mockImplementationOnce(async (original) => acceptedChat(original));
  await f.actions().retryChatMessage(request.requestId!);
  expect(f.send.mock.calls[1][0]).toEqual(request);
});

it("discards a failed message without sending", async () => {
  const f = fixture();
  await f.actions().sendMessage("Original message");
  const request = f.send.mock.calls[0][0];
  await f.actions().discardChatMessage(request.requestId!);
  expect(f.send).toHaveBeenCalledTimes(1);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
});

it("never blocks the next chat message on a failed one, which keeps its own identity", async () => {
  const f = fixture();
  await f.actions().sendMessage("First message");
  f.send.mockImplementationOnce(async (original) => acceptedChat(original));
  await f.actions().sendMessage("Second message");
  const [first, second] = f.send.mock.calls.map(([request]) => request);
  expect(second.requestId).not.toBe(first.requestId);
  expect(loadChatDrafts().chatDrafts.map((draft) => draft.requestId)).toEqual([
    first.requestId,
  ]);
});
