import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  ChatSendResult,
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { createChatActions } from "../src/hooks/chatActions";
import {
  CHAT_SEND_DEADLINE_MS,
  createChatDelivery,
  observeLandedChatSends,
} from "../src/hooks/chatDelivery";
import { createChatStore } from "../src/hooks/chatStore";
import { loadChatDrafts, chatDraftOf, saveChatDraft } from "../src/hooks/chatDrafts";
import { writeSession } from "../src/hooks/sessionStore";
import { deliveryAffordances, sendStatus } from "../src/hooks/chatDeliveryView";
import { admittingProjection, shellStores } from "./shell-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { acceptedChat } from "./chat-delivery-fixture";
import { deferred } from "./deferred";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

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
  expect(sendStatus(draft, Date.now())).toBe("pending");
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
  expect(deliveryAffordances(sendStatus(failed, Date.now()), false)).toEqual({
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
  vi.useFakeTimers();
  const f = fixture();
  const lost = deferred<ChatSendResult>();
  f.send.mockImplementationOnce(() => lost.promise);
  void f.actions().sendMessage("Original message");
  await vi.waitFor(() => expect(f.send).toHaveBeenCalledTimes(1));
  const request = f.send.mock.calls[0][0];
  f.stores.chat = createChatStore();
  const [restored] = loadChatDrafts().chatDrafts;
  // Its saved deadline decides: pending until it passes, then failed.
  expect(sendStatus(restored, Date.now())).toBe("pending");
  expect(sendStatus(restored, restored.pendingUntil!)).toBe("failed");
  await vi.advanceTimersByTimeAsync(CHAT_SEND_DEADLINE_MS);
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

it("fails a hung send at its deadline, and its late confirmation still confirms it", async () => {
  vi.useFakeTimers();
  const f = fixture();
  const hung = deferred<ChatSendResult>();
  f.send.mockImplementationOnce(() => hung.promise);
  const sent = f.actions().sendMessage("Original message");
  await vi.waitFor(() => expect(f.send).toHaveBeenCalledTimes(1));
  const request = f.send.mock.calls[0][0];
  await vi.advanceTimersByTimeAsync(CHAT_SEND_DEADLINE_MS);
  expect(await sent).toBeNull();
  const [failed] = loadChatDrafts().chatDrafts;
  expect(sendStatus(failed, Date.now())).toBe("failed");
  expect(f.report).toHaveBeenCalledWith(expect.stringContaining("No reply arrived"));
  hung.resolve(acceptedChat(request));
  await vi.waitFor(() => expect(loadChatDrafts().chatDrafts).toEqual([]));
});

it("keeps a confirmed message confirmed when the chat view cannot show it", async () => {
  const f = fixture();
  f.send.mockImplementationOnce(async (original) => acceptedChat(original));
  const delivery = createChatDelivery(
    { sendChatMessage: f.send } as unknown as DesktopApiAdapter,
    f.stores,
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

it("reads a confirmed draft saved by the earlier client as confirmed and releases it", () => {
  const request = {
    clientDid: "client",
    nodeDid: "node",
    requestId: "legacy",
    sessionId: "session",
    content: "Original",
  };
  localStorage.setItem(
    `gents.chat.sendDraft.v1:${JSON.stringify(["client", "node", "legacy"])}`,
    JSON.stringify({
      ...request,
      selectedSessionId: "session",
      queued: false,
      requestJson: JSON.stringify(request),
      result: acceptedChat(request),
    }),
  );
  localStorage.setItem("gents.chat.unresolvedSubmissions.v1", "[]");
  expect(loadChatDrafts()).toEqual({ chatDrafts: [], chatDraftError: null });
  expect(localStorage.length).toBe(0);
});

it("clears the composer error about a hung send when its late confirmation arrives", async () => {
  vi.useFakeTimers();
  const f = fixture();
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
});

function showing(requestId: string, sessionId = "session") {
  return {
    sessionId,
    nodeDid: "node",
    latestRequestId: "later",
    pendingTurn: null,
    queuedTurns: [],
    foldedInputs: [],
    timelineItems: [{ kind: "userMessage", requestId, inputRequestId: null }],
  } as unknown as DesktopSessionSnapshot;
}

it("retires a failed send, and its composer error, once the session shows its request", async () => {
  vi.useFakeTimers();
  const f = fixture();
  const stop = observeLandedChatSends(f.stores);
  await f.actions().sendMessage("Original message");
  const request = f.send.mock.calls[0][0];
  expect(f.stores.chat.getState().submissionError?.requestId).toBe(request.requestId);
  writeSession(f.stores.session, showing(request.requestId!, "another-session"));
  writeSession(f.stores.session, showing("another-request"));
  await vi.advanceTimersByTimeAsync(0);
  expect(loadChatDrafts().chatDrafts).toHaveLength(1);
  writeSession(f.stores.session, showing(request.requestId!));
  await vi.advanceTimersByTimeAsync(0);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
  expect(f.stores.chat.getState().submissionError).toBeNull();
  expect(f.send).toHaveBeenCalledTimes(1);
  stop();
});

it("retires a send restored after a restart once its deadline passes and the session shows it", async () => {
  vi.useFakeTimers();
  const f = fixture();
  const draft = chatDraftOf(
    {
      clientDid: "client",
      nodeDid: "node",
      requestId: "restored",
      sessionId: "session",
      content: "Original",
    },
    "session",
  );
  saveChatDraft(f.stores.chat, { ...draft, pendingUntil: Date.now() + 1_000 });
  f.stores.chat = createChatStore();
  const stop = observeLandedChatSends(f.stores);
  writeSession(f.stores.session, showing("restored"));
  await vi.advanceTimersByTimeAsync(0);
  // Pending: its reply, if this window still waits for one, applies first.
  expect(loadChatDrafts().chatDrafts).toHaveLength(1);
  await vi.advanceTimersByTimeAsync(1_000);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
  stop();
});

it("keeps an observed answer saved as sent until its question closes", async () => {
  vi.useFakeTimers();
  const f = fixture();
  const answer = chatDraftOf(
    {
      clientDid: "client",
      nodeDid: "node",
      requestId: "answer",
      sessionId: "session",
      content: "Yes",
      causedBySourceDocId: "question",
    },
    "session",
    false,
    { nodeDid: "node", itemId: "question", summary: "Yes" },
  );
  saveChatDraft(f.stores.chat, {
    ...answer,
    failure: { message: "Outer reply was lost", error: null },
  });
  const stop = observeLandedChatSends(f.stores);
  writeSession(f.stores.session, showing("answer"));
  await vi.advanceTimersByTimeAsync(0);
  const [kept] = loadChatDrafts().chatDrafts;
  expect(sendStatus(kept, Date.now())).toBe("confirmed");
  expect(kept.failure).toBeUndefined();
  stop();
});
