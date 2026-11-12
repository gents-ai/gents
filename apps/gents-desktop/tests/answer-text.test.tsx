import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type {
  DesktopSessionSnapshot,
  QuestionReplyView,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { AnswerText } from "../src/ui/screens/AnswerText";
import { node, testApp, withApp } from "./app-fixture";

const NODE = "did:key:node";

const payload = JSON.stringify({
  version: 2,
  prompt: "Which colour should the button be?",
  options: [
    { id: "red", label: "Red" },
    { id: "blue", label: "Blue" },
  ],
  multi_select: true,
  allow_free_text: true,
  optional: true,
});

/* the agent's question, where the transcript holds it */
const asked = {
  itemKey: "ask",
  toolName: "file_mailbox_item",
  statusKind: "success",
  requestId: "request-1",
  toolCallId: "call-ask",
  presentation: {
    kind: "generic",
    summary: null,
    input: "{}",
    output: JSON.stringify({
      item: { _docID: "item-1", kind: "ask", action: "start_request", payload },
      outcome: "created",
    }),
  },
  reconstruction: { state: "ready" },
} as unknown as RenderedToolCallView;

function show(reply: QuestionReplyView, tools: RenderedToolCallView[] = [asked]) {
  const app = testApp({
    deployments: [node({ nodeDid: NODE })],
    selection: { nodeDid: NODE, sessionId: "session-1" },
    session: {
      sessionId: "session-1",
      latestRequestId: "request-1",
      timelineItems: [{ kind: "toolGroup", itemKey: "g", messageSequence: 1, tools }],
    } as unknown as DesktopSessionSnapshot,
  });
  render(<AnswerText reply={reply} fallback={<span>Answer text</span>} />, {
    wrapper: withApp(app),
  });
}

describe("a message that answers a question", () => {
  it("names the question and the choices made, with any written answer", () => {
    show({
      sourceDocId: "item-1",
      answer: { option_ids: ["blue", "red"], free_text: "Blue for links" },
    });
    expect(screen.getByTestId("question-answer")).toHaveTextContent(
      "Answered “Which colour should the button be?”",
    );
    expect(
      screen.getAllByTestId("question-answer-choice").map((c) => c.textContent),
    ).toEqual(["Blue", "Red"]);
    expect(screen.getByText("Blue for links")).toBeVisible();
    expect(screen.queryByText("Answer text")).toBeNull();
  });

  it("says a skipped question was skipped", () => {
    show({ sourceDocId: "item-1", answer: { option_ids: [], skip: true } });
    expect(screen.getByTestId("question-answer")).toHaveTextContent("Skipped");
    expect(screen.queryByTestId("question-answer-choice")).toBeNull();
  });

  it("keeps the message's text when its question isn't loaded", () => {
    show({ sourceDocId: "item-elsewhere", answer: { option_ids: ["blue"] } });
    expect(screen.queryByTestId("question-answer")).toBeNull();
    expect(screen.getByText("Answer text")).toBeVisible();
  });

  it("keeps the message's text when the answer isn't recorded", () => {
    show({ sourceDocId: "item-1", answer: null });
    expect(screen.getByText("Answer text")).toBeVisible();
  });
});
