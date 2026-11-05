import { acceptedChatReply } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { QuestionPanel } from "../src/ui/screens/session/QuestionPanel";
import { node, testApp, withApp } from "./app-fixture";

const ask = (
  itemId: string,
  question: Record<string, unknown>,
  over: Partial<MailboxItemView> = {},
): MailboxItemView => ({
  itemId,
  itemKey: itemId,
  requesterDid: "did:key:person",
  nodeDid: "did:key:node",
  status: "open",
  kind: "ask",
  action: "start_request",
  title: "Question",
  summary: null,
  payload: JSON.stringify({
    version: 1,
    prompt: "Which backend should the crew use?",
    options: [
      { id: "local", label: "Local model", description: "Runs on this Mac" },
      { id: "claude", label: "Claude" },
    ],
    multi_select: false,
    allow_free_text: true,
    ...question,
  }),
  sourceKind: "session",
  sourceId: "session-1",
  sessionId: "session-1",
  requestId: "request-1",
  causeDocId: null,
  targetNodeDid: "did:key:node",
  targetAgentDid: "engineer",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: new Date().toISOString(),
  ...over,
});

const renderPanel = (items: MailboxItemView[], send = vi.fn(acceptedChatReply)) => {
  const onTypeInstead = vi.fn();
  const app = testApp({
    api: {
      sendChatMessage: send,
      acknowledgeChatSubmission: vi.fn().mockResolvedValue(undefined),
    },
    deployments: [
      node({
        nodeDid: "did:key:node",
        mailboxItems: items,
        agents: [{ agentDid: "engineer", displayName: "The Engineer" }],
        sessions: [{ sessionId: "session-1", title: "Release" }],
      }),
    ],
  });
  render(<QuestionPanel items={items} onTypeInstead={onTypeInstead} />, {
    wrapper: withApp(app),
  });
  return { send, onTypeInstead, panel: screen.getByTestId("question-panel") };
};

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());

describe("the question panel", () => {
  it("names the asking agent and sends a choice as soon as its number is pressed", () => {
    const { send, panel } = renderPanel([ask("q1", {})]);
    expect(screen.getByText("The Engineer is asking")).toBeVisible();
    expect(screen.getByText("Runs on this Mac")).toBeVisible();
    fireEvent.keyDown(panel, { key: "2" });
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({
        causedBySourceDocId: "q1",
        answer: { option_ids: ["claude"], free_text: null },
      }),
    );
  });

  it("sends a clicked choice at once", () => {
    const { send } = renderPanel([ask("q1b", {})]);
    fireEvent.click(screen.getByRole("radio", { name: /Local model/ }));
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({
        answer: { option_ids: ["local"], free_text: null },
      }),
    );
  });

  it("collects several choices where several may be chosen, sending them from Send", () => {
    const { send, panel } = renderPanel([ask("q1c", { multi_select: true })]);
    expect(screen.getByText("Choose any that apply")).toBeVisible();
    fireEvent.keyDown(panel, { key: "1" });
    fireEvent.keyDown(panel, { key: "2" });
    expect(send).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({
        answer: { option_ids: ["local", "claude"], free_text: null },
      }),
    );
  });

  it("writes another answer in its last row, sent from Enter", () => {
    const { send, panel } = renderPanel([ask("q2", {})]);
    fireEvent.keyDown(panel, { key: "3" });
    const field = screen.getByRole("textbox", { name: "Something else" });
    expect(field).toHaveFocus();
    expect(field).toHaveValue("");
    fireEvent.change(field, { target: { value: "A staging box" } });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({
        answer: { option_ids: [], free_text: "A staging box" },
      }),
    );
  });

  it("copies a choice into the field to change it, sending the written text", () => {
    const { send } = renderPanel([ask("q2b", {})]);
    fireEvent.click(screen.getByRole("button", { name: "Edit Claude" }));
    const field = screen.getByRole("textbox", { name: "Something else" });
    expect(field).toHaveValue("Claude");
    expect(send).not.toHaveBeenCalled();
    fireEvent.change(field, { target: { value: "Claude, with a local fallback" } });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({
        answer: { option_ids: [], free_text: "Claude, with a local fallback" },
      }),
    );
  });

  it("offers no pencil where only the choices are answers", () => {
    renderPanel([ask("q2c", { allow_free_text: false })]);
    expect(screen.queryByRole("button", { name: "Edit Claude" })).toBeNull();
  });

  it("gives the composer back on Escape or its close button, the question still open", () => {
    const { onTypeInstead, panel } = renderPanel([ask("q3", {})]);
    fireEvent.keyDown(panel, { key: "Escape" });
    fireEvent.click(screen.getByRole("button", { name: "Type a message instead" }));
    expect(onTypeInstead).toHaveBeenCalledTimes(2);
  });

  it("shows one of several questions at a time, with arrows between them", () => {
    renderPanel([
      ask("q4", {}),
      ask("q5", {
        prompt: "Which region?",
        options: [
          { id: "eu", label: "EU" },
          { id: "us", label: "US" },
        ],
      }),
    ]);
    expect(screen.getByText("1 of 2")).toBeVisible();
    expect(screen.getByText("Which backend should the crew use?")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Next question" }));
    expect(screen.getByText("2 of 2")).toBeVisible();
    expect(screen.getByText("Which region?")).toBeVisible();
  });

  it("offers Skip for an optional question until another answer is written", () => {
    const { send } = renderPanel([
      ask("q6", { version: 2, optional: true }, { questionHash: "hash-q6" }),
    ]);
    const field = screen.getByRole("textbox", { name: "Something else" });
    fireEvent.change(field, { target: { value: "Neither" } });
    expect(screen.queryByRole("button", { name: "Skip" })).toBeNull();
    expect(screen.getByRole("button", { name: "Send" })).toBeEnabled();
    fireEvent.change(field, { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "Skip" }));
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({
        answer: {
          option_ids: [],
          free_text: null,
          skip: true,
          question_hash: "hash-q6",
        },
      }),
    );
  });

  it("leaves Enter on a focused button to that button", async () => {
    const user = userEvent.setup();
    const { send, onTypeInstead } = renderPanel([
      ask("q8", { multi_select: true }),
      ask("q9", { version: 2, optional: true }, { questionHash: "hash-q9" }),
    ]);
    const local = screen.getByRole("checkbox", { name: /Local model/ });
    local.focus();
    await user.keyboard("{Enter}");
    expect(local).toHaveAttribute("aria-checked", "true");
    expect(send).not.toHaveBeenCalled();
    screen.getByRole("button", { name: "Next question" }).focus();
    await user.keyboard("{Enter}");
    expect(screen.getByText("2 of 2")).toBeVisible();
    expect(send).not.toHaveBeenCalled();
    screen.getByRole("button", { name: "Skip" }).focus();
    await user.keyboard("{Enter}");
    expect(send).toHaveBeenCalledWith(
      expect.objectContaining({
        causedBySourceDocId: "q9",
        answer: expect.objectContaining({ skip: true }),
      }),
    );
    expect(onTypeInstead).not.toHaveBeenCalled();
  });

  it("leaves the keys to a field the person is writing in when it opens", () => {
    const other = document.createElement("textarea");
    document.body.append(other);
    other.focus();
    const { send } = renderPanel([ask("q10", {})]);
    expect(other).toHaveFocus();
    other.remove();
    expect(send).not.toHaveBeenCalled();
  });

  it("offers no Skip for a required question, only Send once something is written", () => {
    renderPanel([ask("q7", { version: 2 }, { questionHash: "hash-q7" })]);
    expect(screen.queryByRole("button", { name: "Skip" })).toBeNull();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
  });

  it("leaves out the last row when a click is the only way to answer", () => {
    renderPanel([ask("q8", { allow_free_text: false })]);
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.getByRole("radio", { name: /Claude/ })).toBeVisible();
  });
});
