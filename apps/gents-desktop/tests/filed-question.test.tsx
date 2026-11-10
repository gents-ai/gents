import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type {
  MailboxItemView,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { FiledQuestion, filedQuestion } from "../src/ui/screens/FiledQuestion";
import { node, testApp, withApp } from "./app-fixture";

const payload = JSON.stringify({
  version: 2,
  prompt: "Which colour should the button be?",
  options: [
    { id: "red", label: "Red", description: "Matches the logo" },
    { id: "blue", label: "Blue" },
  ],
  multi_select: false,
  allow_free_text: true,
  optional: true,
});

const filing = (item: Record<string, unknown> | null, toolName = "file_mailbox_item") =>
  ({
    toolName,
    statusKind: "success",
    presentation: {
      kind: "generic",
      summary: null,
      input: "{}",
      output: item ? JSON.stringify({ item, outcome: "created" }) : null,
    },
  }) as unknown as RenderedToolCallView;

const stored = { _docID: "item-1", kind: "ask", action: "start_request", payload };

const show = (open: boolean) => {
  const asked = filedQuestion(filing(stored))!;
  const app = testApp({
    deployments: [
      node({
        nodeDid: "did:key:node",
        mailboxItems: open
          ? [
              {
                itemId: "item-1",
                kind: "ask",
                action: "start_request",
                status: "open",
                payload,
                sessionId: "session-1",
                targetNodeDid: "did:key:node",
                createdAt: new Date().toISOString(),
              } as MailboxItemView,
            ]
          : [],
      }),
    ],
  });
  render(<FiledQuestion itemId={asked.itemId} question={asked.question} />, {
    wrapper: withApp(app),
  });
};

describe("a question in the transcript", () => {
  it("reads the question from the stored item in the tool's result", () => {
    expect(filedQuestion(filing(stored))).toMatchObject({
      itemId: "item-1",
      question: { prompt: "Which colour should the button be?" },
    });
    expect(filedQuestion(filing({ ...stored, kind: "finished" }))).toBeNull();
    expect(filedQuestion(filing(null))).toBeNull();
    expect(filedQuestion(filing(stored, "web_request"))).toBeNull();
  });

  it("shows the prompt, the choices and how it may be answered", () => {
    show(false);
    expect(screen.getByText("Which colour should the button be?")).toBeVisible();
    expect(screen.getByText("Red")).toBeVisible();
    expect(screen.getByText("Matches the logo")).toBeVisible();
    expect(screen.getByText("Blue")).toBeVisible();
    expect(screen.getByText("Or a written answer · Optional")).toBeVisible();
  });

  it("says it is waiting while the question is open", () => {
    show(true);
    expect(screen.getByText(/Waiting for your answer/)).toBeVisible();
  });
});
