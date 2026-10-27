import { fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  ChatSendResult,
  ChatSubmissionRecovery,
  MailboxItemView,
} from "@source-inc/gents-desktop-client";
import { testApp, node, withApp } from "./app-fixture";
import { acceptedChat } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { loadChatDrafts } from "../src/hooks/chatDrafts";
import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { ChatDeliveryFeedback } from "../src/ui/screens/session/ChatDeliveryFeedback";
import { selection } from "../src/hooks/selectionStore";
import { deferred } from "./deferred";

const NODE = "did:key:node";
beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());

function item(id = "question"): MailboxItemView {
  return {
    itemId: id,
    itemKey: id,
    requesterDid: "test-client",
    nodeDid: NODE,
    status: "open",
    kind: "ask",
    action: "start_request",
    title: `Question ${id}`,
    summary: null,
    payload: JSON.stringify({
      version: 1,
      prompt: "Where should this run?",
      options: [
        { id: "local", label: "This computer" },
        { id: "remote", label: "Remote host" },
      ],
      multi_select: true,
      allow_free_text: true,
    }),
    sourceKind: "session",
    sourceId: "session",
    sessionId: "session",
    requestId: null,
    graphRunId: null,
    causeDocId: null,
    targetNodeDid: NODE,
    targetAgentId: "engineer",
    expectedCollection: null,
    parentItemId: null,
    deadlineAt: null,
    createdAt: "2026-10-27T10:00:00Z",
  };
}
const original = { option_ids: ["local"], free_text: "Keep the workspace local" };

function fixture(items = [item()]) {
  const send = vi
    .fn<(request: ChatSendRequest) => Promise<ChatSendResult>>()
    .mockRejectedValue(new Error("Outer reply was lost"));
  const read = vi.fn<() => Promise<ChatSubmissionRecovery>>();
  const ack = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
  const report = vi.fn();
  const refresh = vi.fn().mockRejectedValue(new Error("Snapshot unavailable"));
  const dismiss = vi.fn().mockResolvedValue(undefined);
  const app = testApp({
    api: {
      sendChatMessage: send,
      recoverChatSubmission: read,
      acknowledgeChatSubmission: ack,
      fetchDesktopSnapshot: refresh,
      dismissMailboxItem: dismiss,
    },
    deployments: [node({ nodeDid: NODE, mailboxItems: items })],
    selection: { nodeDid: NODE, sessionId: "session" },
    reportFailure: report,
  });
  return { app, send, read, ack, report, refresh, dismiss };
}

it("freezes the structured answer, question and caller identity before dispatch", async () => {
  const f = fixture();
  f.send.mockImplementationOnce(async (request) => {
    const draft = loadChatDrafts().chatDrafts[0];
    expect(JSON.parse(draft.requestJson)).toEqual(request);
    expect(draft.mailbox).toEqual({
      itemId: "question",
      summary: "This computer\nKeep the workspace local",
    });
    expect(request).toMatchObject({
      clientDid: "test-client",
      requestId: expect.any(String),
      nodeDid: NODE,
      sessionId: "session",
      agentId: "engineer",
      causedBySourceDocId: "question",
      content: "",
      answer: original,
    });
    throw new Error("Lost reply");
  });
  await expect(f.app.actions.answerMailboxQuestion(item(), original)).rejects.toThrow(
    "Lost reply",
  );
  expect(f.send).toHaveBeenCalledTimes(1);
  expect(f.ack).not.toHaveBeenCalled();
});

it("reloads a lost answer, checks without publication and resumes the original choices", async () => {
  const f = fixture();
  await f.app.actions.answerMailboxQuestion(item(), original).catch(() => {});
  const request = f.send.mock.calls[0][0];
  const reloaded = fixture();
  reloaded.app.drafts.setState({ drafts: { newer: "Keep this newer input" } });
  reloaded.read.mockResolvedValue({
    kind: "unconfirmed",
    nodeDid: NODE,
    requestId: request.requestId!,
  });
  expect(
    await reloaded.app.actions.recoverMailboxAnswer("question", request.requestId!),
  ).toBeNull();
  expect(reloaded.send).not.toHaveBeenCalled();
  await expect(
    reloaded.app.actions.answerMailboxQuestion(item(), {
      option_ids: ["remote"],
      free_text: null,
    }),
  ).rejects.toThrow("Check the saved answer");
  expect(reloaded.send).not.toHaveBeenCalled();
  reloaded.send.mockImplementationOnce(async (request) => acceptedChat(request));
  await reloaded.app.actions.recoverMailboxAnswer("question", request.requestId!, true);
  expect(reloaded.send).toHaveBeenCalledExactlyOnceWith(request);
  expect(reloaded.app.drafts.getState().drafts.newer).toBe("Keep this newer input");
  await vi.waitFor(() => expect(reloaded.ack).toHaveBeenCalledTimes(1));
  expect(loadChatDrafts().chatDrafts[0].result).toEqual(acceptedChat(request));
});

it("does not mark an unknown delivery as Answer sent or erase the user's choices", async () => {
  const f = fixture();
  f.send.mockImplementationOnce(async (request) => ({
    ...acceptedChat(request),
    outcome: "deliveryUnknown",
  }));
  render(<MailboxScreen />, { wrapper: withApp(f.app) });
  fireEvent.click(screen.getByRole("button", { name: "This computer" }));
  fireEvent.change(screen.getByLabelText("Other"), {
    target: { value: "Keep the workspace local" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await screen.findByText(
    "Your answer is not confirmed. Use Check answer delivery or Resume saved answer below.",
  );
  expect(screen.queryByText("Answer sent to the agent.")).toBeNull();
  expect(screen.getByLabelText("Other")).toHaveValue("Keep the workspace local");
  expect(screen.getByRole("button", { name: "This computer" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Check answer delivery" })).toBeEnabled();
});

it("keeps Check and Resume available after reloading an uncertain question", async () => {
  const f = fixture();
  await f.app.actions.answerMailboxQuestion(item(), original).catch(() => {});
  const request = f.send.mock.calls[0][0];
  const reloaded = fixture();
  reloaded.read.mockResolvedValue({
    kind: "unconfirmed",
    nodeDid: NODE,
    requestId: request.requestId!,
  });
  reloaded.send.mockImplementationOnce(async (request) => acceptedChat(request));
  render(<MailboxScreen />, { wrapper: withApp(reloaded.app) });
  const panel = screen.getByRole("status", { name: "Saved answer" });
  fireEvent.click(within(panel).getByText("Saved answer"));
  expect(within(panel).getByText("This computer", { exact: false })).toHaveTextContent(
    "Keep the workspace local",
  );
  fireEvent.click(within(panel).getByRole("button", { name: "Check answer delivery" }));
  await within(panel).findByText(
    "Delivery is still unconfirmed. Check again or resume the saved answer.",
  );
  expect(reloaded.send).not.toHaveBeenCalled();
  fireEvent.click(within(panel).getByRole("button", { name: "Resume saved answer" }));
  await screen.findByText("Answer sent to the agent.");
  expect(reloaded.send).toHaveBeenCalledExactlyOnceWith(request);
  expect(screen.queryByRole("button", { name: "This computer" })).toBeNull();
});

it("retains the signed source for an optional skip whose delivery is uncertain", async () => {
  const question = {
    ...item(),
    questionHash: "observed-question",
    payload: JSON.stringify({
      ...JSON.parse(item().payload!),
      version: 2,
      optional: true,
    }),
  };
  const f = fixture([question]);
  f.send.mockImplementationOnce(async (request) => ({
    ...acceptedChat(request),
    outcome: "deliveryUnknown",
  }));
  render(<MailboxScreen />, { wrapper: withApp(f.app) });
  fireEvent.click(screen.getByRole("button", { name: "Skip" }));

  await screen.findByText(
    "Your answer is not confirmed. Use Check answer delivery or Resume saved answer below.",
  );
  expect(f.send.mock.calls[0][0].answer).toEqual({
    option_ids: [],
    free_text: null,
    skip: true,
    question_hash: "observed-question",
  });
  expect(screen.queryByText("Question skipped.")).toBeNull();
  expect(screen.getByRole("button", { name: "Skip" })).toBeDisabled();
  expect(JSON.parse(loadChatDrafts().chatDrafts[0].requestJson).answer).toEqual(
    f.send.mock.calls[0][0].answer,
  );
});

it("retains confirmed acceptance across reload while the native question is still open", async () => {
  const f = fixture();
  f.send.mockImplementationOnce(async (request) => acceptedChat(request));
  expect(await f.app.actions.answerMailboxQuestion(item(), original)).toBe(true);
  await vi.waitFor(() => expect(f.ack).toHaveBeenCalledTimes(1));
  const reloaded = fixture();
  render(<MailboxScreen />, { wrapper: withApp(reloaded.app) });
  expect(screen.getByText("Answer sent to the agent.")).toBeVisible();
  expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
  await expect(
    reloaded.app.actions.answerMailboxQuestion(item(), original),
  ).rejects.toThrow("Check the saved answer");
  expect(reloaded.send).not.toHaveBeenCalled();
});

it("recovers the answer from its chat even if the question card has disappeared", async () => {
  const f = fixture();
  await f.app.actions.answerMailboxQuestion(item(), original).catch(() => {});
  const request = f.send.mock.calls[0][0];
  const reloaded = fixture([]);
  reloaded.read.mockResolvedValue({ kind: "prepared", result: acceptedChat(request) });
  render(<ChatDeliveryFeedback />, { wrapper: withApp(reloaded.app) });
  fireEvent.click(screen.getByRole("button", { name: "Check answer delivery" }));
  await screen.findByText("Answer sent");
  expect(reloaded.send).not.toHaveBeenCalled();
});

it("does not confuse two pending questions in the same session", async () => {
  const f = fixture([item("first"), item("second")]);
  const first = deferred<ChatSendResult>();
  f.send
    .mockReturnValueOnce(first.promise)
    .mockImplementationOnce(async (request) => acceptedChat(request));
  const pending = f.app.actions.answerMailboxQuestion(item("first"), original);
  expect(await f.app.actions.answerMailboxQuestion(item("second"), original)).toBe(
    true,
  );
  expect(f.send).toHaveBeenCalledTimes(2);
  first.resolve(acceptedChat(f.send.mock.calls[0][0]));
  await pending;
  expect(f.send.mock.calls[0][0].requestId).not.toBe(f.send.mock.calls[1][0].requestId);
  expect(
    loadChatDrafts()
      .chatDrafts.map((draft) => draft.mailbox?.itemId)
      .sort(),
  ).toEqual(["first", "second"]);
});

it("does not acknowledge an answer before its confirmation is saved", async () => {
  const f = fixture();
  f.send.mockImplementationOnce(async (request) => {
    vi.spyOn(localStorage, "setItem").mockImplementation(() => {
      throw new Error("Storage full");
    });
    return acceptedChat(request);
  });
  await expect(f.app.actions.answerMailboxQuestion(item(), original)).rejects.toThrow(
    "Storage full",
  );
  expect(f.ack).not.toHaveBeenCalled();
});

it("does not let a late answer release a replacement compose route", async () => {
  const f = fixture();
  const reply = deferred<ChatSendResult>();
  f.send.mockReturnValueOnce(reply.promise);
  const pending = f.app.actions.answerMailboxQuestion(item(), original);
  const request = f.send.mock.calls[0][0];
  selection.advanceIntent(f.app.stores.selection);
  selection.openMailboxRoute(f.app.stores.selection, {
    itemId: "another-question",
    nodeDid: NODE,
    agentId: "engineer",
    sessionId: "session",
  });
  reply.resolve(acceptedChat(request));
  expect(await pending).toBe(true);
  expect(f.app.stores.selection.getState().mailboxRoute?.itemId).toBe(
    "another-question",
  );
});

it("does not turn a failed snapshot refresh into a failed answer", async () => {
  const f = fixture();
  f.send.mockImplementationOnce(async (request) => acceptedChat(request));
  expect(await f.app.actions.answerMailboxQuestion(item(), original)).toBe(true);
  await vi.waitFor(() => expect(f.refresh).toHaveBeenCalled());
  expect(f.report).not.toHaveBeenCalled();
});

it("rejects recovery from another question without querying or publishing", async () => {
  const f = fixture();
  await f.app.actions.answerMailboxQuestion(item(), original).catch(() => {});
  await expect(
    f.app.actions.recoverMailboxAnswer(
      "other-question",
      f.send.mock.calls[0][0].requestId!,
      true,
    ),
  ).rejects.toThrow("another question");
  expect(f.read).not.toHaveBeenCalled();
  expect(f.send).toHaveBeenCalledTimes(1);
});

it("retires a confirmed form only after explicit question dismissal and acknowledgement", async () => {
  const f = fixture();
  f.send.mockImplementationOnce(async (request) => acceptedChat(request));
  await f.app.actions.answerMailboxQuestion(item(), original);
  await vi.waitFor(() => expect(f.ack).toHaveBeenCalledTimes(1));
  expect(loadChatDrafts().chatDrafts).toHaveLength(1);
  await expect(f.app.actions.dismissMailboxItem("question")).resolves.toBeUndefined();
  expect(f.dismiss).toHaveBeenCalledExactlyOnceWith("question");
  expect(loadChatDrafts().chatDrafts).toHaveLength(0);
});
