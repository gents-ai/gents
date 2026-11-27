import { act, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  MailboxItemObservation,
  MailboxItemView,
} from "@source-inc/gents-desktop-client";
import { testApp, node, publish, publishSnapshot } from "./app-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { deferred } from "./deferred";
import { loadChatDrafts, chatDraftOf, saveChatDraft } from "../src/hooks/chatDrafts";

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
  targetAgentDid: "agent",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: "2026-10-28T00:00:00Z",
};
const closed = (status = "acted"): MailboxItemObservation => ({
  item: { ...question, status },
  terminal: true,
});
function answer(requestId = "answer", sourceNodeDid = NODE) {
  const request: ChatSendRequest = {
    clientDid: "test-client",
    nodeDid: NODE,
    agentDid: "agent",
    sessionId: "session",
    requestId,
    causedBySourceDocId: "question",
    content: "",
    answer: { option_ids: ["yes"] },
  };
  return {
    ...chatDraftOf(request, "session", false, {
      nodeDid: sourceNodeDid,
      itemId: "question",
      summary: "Yes",
    }),
    confirmed: true as const,
  };
}
function fixture(items: MailboxItemView[] = []) {
  const observe = vi.fn().mockResolvedValue(closed());
  const app = testApp({
    api: { observeMailboxItem: observe },
    deployments: [node({ nodeDid: NODE, mailboxItems: items })],
  });
  return { app, observe };
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
    nodeDid: NODE,
    itemId: "question",
  });
  expect(loadChatDrafts().chatDrafts).toHaveLength(1);
  publish(f.app, [node({ nodeDid: NODE, mailboxItems: [question] })]);
  await flush();
  publish(f.app, [node({ nodeDid: NODE })]);
  await waitFor(() => expect(loadChatDrafts().chatDrafts).toHaveLength(0));
});

it("coalesces updates into one closure observation", async () => {
  const f = fixture();
  const observed = deferred<MailboxItemObservation>();
  f.observe.mockReturnValue(observed.promise);
  saveChatDraft(f.app.stores.chat, answer());
  await flush();
  for (let i = 0; i < 10; i++)
    publish(f.app, [node({ nodeDid: NODE, label: String(i) })]);
  await flush();
  expect(f.observe).toHaveBeenCalledTimes(1);
  await act(async () => observed.resolve(closed()));
  expect(loadChatDrafts().chatDrafts).toHaveLength(0);
});

it("fences a client switch during observation", async () => {
  const f = fixture();
  const waiting = deferred<MailboxItemObservation>();
  f.observe.mockReturnValue(waiting.promise);
  saveChatDraft(f.app.stores.chat, answer());
  await flush();
  const snapshot = f.app.stores.client.getState().snapshot!;
  publishSnapshot(f.app, {
    ...snapshot,
    client: { ...snapshot.client!, clientDid: "other-client" },
  });
  await act(async () => waiting.resolve(closed()));
  expect(loadChatDrafts().chatDrafts).toHaveLength(1);
});

it("checks persisted data again after observation and preserves newer or changed answers", async () => {
  const f = fixture();
  const observed = deferred<MailboxItemObservation>();
  f.observe.mockReturnValue(observed.promise);
  saveChatDraft(f.app.stores.chat, answer());
  await waitFor(() => expect(f.observe).toHaveBeenCalledTimes(1));
  const { confirmed: _, ...changed } = answer();
  saveChatDraft(f.app.stores.chat, changed);
  const { confirmed: __, ...newer } = answer("newer");
  saveChatDraft(f.app.stores.chat, newer);
  await act(async () => observed.resolve(closed()));
  expect(loadChatDrafts().chatDrafts.map((row) => row.requestId)).toEqual([
    "answer",
    "newer",
  ]);
  expect(loadChatDrafts().chatDrafts[0].confirmed).toBeUndefined();
});

it("observes closure at the mailbox host when the reply targets another runtime", async () => {
  const source = "source-runtime";
  const hostedQuestion = { ...question, nodeDid: source };
  const f = fixture();
  publish(f.app, [node({ nodeDid: source, mailboxItems: [hostedQuestion] })]);
  saveChatDraft(f.app.stores.chat, answer("answer", source));
  await flush();
  expect(f.observe).not.toHaveBeenCalled();
  publish(f.app, [node({ nodeDid: source })]);
  await flush();
  expect(f.observe).toHaveBeenCalledExactlyOnceWith({
    clientDid: "test-client",
    nodeDid: source,
    itemId: "question",
  });
  expect(loadChatDrafts().chatDrafts).toHaveLength(1);
  f.observe.mockResolvedValue({
    item: { ...hostedQuestion, status: "acted" },
    terminal: true,
  });
  publish(f.app, [node({ nodeDid: source, mailboxItems: [hostedQuestion] })]);
  await flush();
  publish(f.app, [node({ nodeDid: source })]);
  await waitFor(() => expect(loadChatDrafts().chatDrafts).toHaveLength(0));
});
