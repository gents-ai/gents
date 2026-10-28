import { act, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  MailboxItemObservation,
  MailboxItemView,
} from "@source-inc/gents-desktop-client";
import { testApp, node, publish, publishSnapshot } from "./app-fixture";
import { acceptedChat } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { deferred } from "./deferred";
import {
  loadChatDrafts,
  retainChatDraft,
  saveChatDraft,
} from "../src/hooks/chatDrafts";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());
const NODE = "node";
const question: MailboxItemView = {
  itemId: "question",
  itemKey: "question",
  requesterDid: "test-client",
  nodeDid: NODE,
  status: "open",
  kind: "ask",
  action: "start_request",
  title: "Question",
  summary: null,
  payload: null,
  sourceKind: "session",
  sourceId: "session",
  sessionId: "session",
  requestId: null,
  causeDocId: null,
  targetNodeDid: NODE,
  targetAgentId: "agent",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: "2026-10-28T00:00:00Z",
};
const closed = (status = "acted"): MailboxItemObservation => ({
  item: { ...question, status },
  terminal: true,
});
function answer(requestId = "answer") {
  const request: ChatSendRequest = {
    clientDid: "test-client",
    nodeDid: NODE,
    agentId: "agent",
    sessionId: "session",
    requestId,
    causedBySourceDocId: "question",
    content: "",
    answer: { option_ids: ["yes"] },
  };
  return {
    ...retainChatDraft(request, "session", false, {
      itemId: "question",
      summary: "Yes",
    }),
    result: acceptedChat(request),
  };
}
function fixture(items: MailboxItemView[] = []) {
  const observe = vi.fn().mockResolvedValue(closed());
  const ack = vi.fn().mockResolvedValue(undefined);
  const app = testApp({
    api: { observeMailboxItem: observe, acknowledgeChatSubmission: ack },
    deployments: [node({ nodeDid: NODE, mailboxItems: items })],
  });
  return { app, observe, ack };
}
const flush = () => act(async () => {});

it("keeps an open answer latched and requires a positive exact native closure after list disappearance", async () => {
  const f = fixture([question]);
  saveChatDraft(f.app.stores.chat, answer());
  await flush();
  expect(f.observe).not.toHaveBeenCalled();
  f.observe.mockResolvedValueOnce(null);
  publish(f.app, [node({ nodeDid: NODE })]);
  await flush();
  expect(f.observe).toHaveBeenCalledExactlyOnceWith({
    clientDid: "test-client",
    itemId: "question",
  });
  expect(f.ack).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts).toHaveLength(1);
  publish(f.app, [node({ nodeDid: NODE, mailboxItems: [question] })]);
  await flush();
  publish(f.app, [node({ nodeDid: NODE })]);
  await waitFor(() => expect(loadChatDrafts().chatDrafts).toHaveLength(0));
  expect(f.ack).toHaveBeenCalledExactlyOnceWith(answer().result.submissionReceipt);
});

it.each(["acted", "dismissed", "expired"])(
  "retires a restored confirmed answer only after %s and successful acknowledgement",
  async (status) => {
    const waiting = deferred();
    const saved = answer();
    const f = fixture();
    f.observe.mockResolvedValue(closed(status));
    f.ack.mockReturnValue(waiting.promise);
    saveChatDraft(f.app.stores.chat, saved);
    await waitFor(() => expect(f.ack).toHaveBeenCalledTimes(1));
    expect(loadChatDrafts().chatDrafts).toHaveLength(1);
    await act(async () => waiting.resolve());
    expect(loadChatDrafts().chatDrafts).toHaveLength(0);
    expect(f.app.stores.chat.getState().chatDrafts).toHaveLength(0);
  },
);

it.each(["missing", "open", "read-failure", "ack-failure"])(
  "preserves the original resolver after %s",
  async (failure) => {
    const f = fixture();
    if (failure === "missing") f.observe.mockResolvedValue(null);
    if (failure === "open")
      f.observe.mockResolvedValue({ item: question, terminal: false });
    if (failure === "read-failure")
      f.observe.mockRejectedValue(new Error("Unavailable"));
    if (failure === "ack-failure") f.ack.mockRejectedValue(new Error("Reply lost"));
    saveChatDraft(f.app.stores.chat, answer());
    await flush();
    expect(loadChatDrafts().chatDrafts).toEqual([answer()]);
    expect(f.observe).toHaveBeenCalledTimes(1);
    expect(f.ack).toHaveBeenCalledTimes(failure === "ack-failure" ? 1 : 0);
  },
);

it("does not resolve or acknowledge uncertain answers even when their question is closed", async () => {
  const f = fixture();
  const saved = answer();
  saveChatDraft(f.app.stores.chat, {
    ...saved,
    result: { ...saved.result, outcome: "deliveryUnknown" },
  });
  await flush();
  expect(f.observe).not.toHaveBeenCalled();
  expect(f.ack).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts[0].result?.outcome).toBe("deliveryUnknown");
});

it("coalesces updates and waits for in-flight delivery before observing closure", async () => {
  const f = fixture();
  const observed = deferred<MailboxItemObservation>();
  f.observe.mockReturnValue(observed.promise);
  f.app.stores.chat.setState({ chatRequestsInFlight: new Set(["answer"]) });
  saveChatDraft(f.app.stores.chat, answer());
  await flush();
  expect(f.observe).not.toHaveBeenCalled();
  f.app.stores.chat.setState({ chatRequestsInFlight: new Set() });
  await flush();
  for (let i = 0; i < 10; i++)
    publish(f.app, [node({ nodeDid: NODE, label: String(i) })]);
  await flush();
  expect(f.observe).toHaveBeenCalledTimes(1);
  await act(async () => observed.resolve(closed()));
  expect(f.ack).toHaveBeenCalledTimes(1);
  expect(loadChatDrafts().chatDrafts).toHaveLength(0);
});

it.each(["observation", "acknowledgement"])(
  "fences a client switch during %s",
  async (boundary) => {
    const f = fixture();
    const waiting = deferred<MailboxItemObservation>();
    const ack = deferred();
    if (boundary === "observation") f.observe.mockReturnValue(waiting.promise);
    else f.ack.mockReturnValue(ack.promise);
    saveChatDraft(f.app.stores.chat, answer());
    await flush();
    const snapshot = f.app.stores.client.getState().snapshot!;
    publishSnapshot(f.app, {
      ...snapshot,
      client: { ...snapshot.client!, clientDid: "other-client" },
    });
    await act(async () => {
      waiting.resolve(closed());
      ack.resolve();
    });
    expect(loadChatDrafts().chatDrafts).toHaveLength(1);
    expect(f.ack).toHaveBeenCalledTimes(boundary === "acknowledgement" ? 1 : 0);
  },
);

it("checks persisted data again after acknowledgement and preserves newer or changed answers", async () => {
  const f = fixture();
  const ack = deferred();
  f.ack.mockReturnValue(ack.promise);
  saveChatDraft(f.app.stores.chat, answer());
  await waitFor(() => expect(f.ack).toHaveBeenCalledTimes(1));
  const saved = answer();
  saveChatDraft(f.app.stores.chat, {
    ...saved,
    result: { ...saved.result, outcome: "deliveryUnknown" },
  });
  saveChatDraft(f.app.stores.chat, { ...answer("newer"), result: undefined });
  await act(async () => ack.resolve());
  expect(loadChatDrafts().chatDrafts.map((row) => row.requestId)).toEqual([
    "answer",
    "newer",
  ]);
  expect(loadChatDrafts().chatDrafts[0].result?.outcome).toBe("deliveryUnknown");
});

it("reconciles a confirmed answer from saved storage after app recreation without publishing again", async () => {
  const saved = answer();
  const first = fixture([question]);
  saveChatDraft(first.app.stores.chat, saved);
  await flush();
  const restored = fixture();
  await waitFor(() => expect(loadChatDrafts().chatDrafts).toHaveLength(0));
  expect(restored.observe).toHaveBeenCalledExactlyOnceWith({
    clientDid: "test-client",
    itemId: "question",
  });
  expect(restored.ack).toHaveBeenCalledExactlyOnceWith(saved.result.submissionReceipt);
});
