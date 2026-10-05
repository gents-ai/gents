import { fireEvent, render, screen, within } from "@testing-library/react";
import type { ReactElement } from "react";
import { describe, expect, it, vi } from "vitest";

import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { testApp, withApp } from "./app-fixture";

/* the sender's hover card reads the whole deployment; the card is not
   what these cases are about */
vi.mock("../src/ui/screens/HoverCards", () => ({
  BehaviorHoverCard: ({ children }: { children: ReactElement }) => children,
}));

const item = (over: Partial<MailboxItemView> = {}): MailboxItemView => ({
  itemId: "item-1",
  itemKey: "key-1",
  requesterDid: "did:key:person",
  agentDid: "did:key:agent",
  status: "open",
  kind: "finished",
  action: "ack",
  title: "Refactor landed",
  summary: null,
  payload: null,
  sourceKind: "session",
  sourceId: "session-1",
  sessionId: "session-1",
  requestId: null,
  graphRunId: null,
  causeDocId: null,
  targetAgentDid: "did:key:agent",
  targetBehaviorId: "engineer",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: new Date(Date.now() - 5 * 60_000).toISOString(),
  ...over,
});

const shellWith = (
  items: MailboxItemView[],
  answerMailboxQuestion: Shell["answerMailboxQuestion"] = vi.fn(),
) =>
  ({
    answerMailboxQuestion,
    deployments: [deploymentWith(items)],
    app: testApp({ deployments: [deploymentWith(items)] }),
    selectedDeployment: deploymentWith(items),
  }) as unknown as Shell;
const deploymentWith = (items: MailboxItemView[]) => ({
  agentDid: "did:key:node",
  mailboxItems: items,
  behaviors: [{ behaviorId: "engineer", displayName: "Engineer" }],
  sessions: [{ sessionId: "session-1", title: "Mailbox cleanup" }],
});

const renderMailbox = (shell: Shell) =>
  render(<MailboxScreen shell={shell} />, { wrapper: withApp(shell.app) });

describe("mailbox item", () => {
  it("shows the title, sender, session, time, kind and status", () => {
    renderMailbox(shellWith([item()]));
    expect(screen.getByRole("heading", { name: "Refactor landed" })).toBeVisible();
    const meta = screen.getByTestId("mailbox-item-meta");
    expect(within(meta).getByText("Finished")).toBeVisible();
    expect(within(meta).getByText("Open")).toBeVisible();
    expect(within(meta).getByText("Engineer")).toBeVisible();
    const link = within(meta).getByText("in Mailbox cleanup");
    expect(link.closest("a")).toHaveAttribute("href");
    const times = screen.getAllByText("5m");
    expect(times[0]!.tagName).toBe("TIME");
    expect(times[0]).toHaveAttribute("title");
  });

  it("names the source by agent, session and time, never its raw identity", () => {
    const sourceId =
      '["event","did:key:agent","did:key:person","engineer","request-1"]';
    renderMailbox(shellWith([item({ sourceKind: "agent", sourceId })]));
    expect(screen.queryByText(sourceId, { exact: false })).toBeNull();
    expect(screen.queryByText(/did:key:/)).toBeNull();
  });

  it("renders the summary as markdown and keeps its line breaks", () => {
    const summary = "First line\nsecond line\n\n- one\n- two\n\n**bold**";
    renderMailbox(shellWith([item({ summary })]));
    const body = screen.getByTestId("mailbox-item-body");
    expect(body.querySelectorAll("li")).toHaveLength(2);
    expect(body.querySelector("strong")).toHaveTextContent("bold");
    const first = body.querySelector("p")!;
    expect(first.querySelector("br")).not.toBeNull();
    expect(first).toHaveTextContent("First line");
    expect(first).toHaveTextContent("second line");
  });

  it("renders a JSON payload as a code block and a text payload as markdown", () => {
    renderMailbox(
      shellWith([
        item({ itemId: "a", title: "json", payload: '{"pr":42}' }),
        item({ itemId: "b", title: "text", payload: "## Next\n1. review" }),
      ]),
    );
    /* by card, not by position: the list orders items newest first, and
       the two fixtures are created a moment apart */
    const bodyOf = (title: string) =>
      within(
        screen.getByRole("heading", { name: title }).closest("article")!,
      ).getByTestId("mailbox-item-body");
    const json = bodyOf("json");
    const text = bodyOf("text");
    expect(json.querySelector("pre")).toHaveTextContent('"pr": 42');
    expect(text.querySelector("h2")).toHaveTextContent("Next");
    expect(text.querySelector("ol li")).toHaveTextContent("review");
  });

  it("folds a long body behind show more and unfolds it", () => {
    const summary = Array.from({ length: 20 }, (_, i) => `line ${i}`).join("\n");
    renderMailbox(shellWith([item({ summary })]));
    const body = screen.getByTestId("mailbox-item-body");
    expect(body).toHaveClass("max-h-48");
    const more = screen.getByRole("button", { name: "Show more" });
    expect(more).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(more);
    expect(body).not.toHaveClass("max-h-48");
    const less = screen.getByRole("button", { name: "Show less" });
    expect(less).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(less);
    expect(body).toHaveClass("max-h-48");
  });

  it("offers no fold for a short body", () => {
    renderMailbox(shellWith([item({ summary: "short" })]));
    expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
    expect(screen.getByTestId("mailbox-item-body")).not.toHaveClass("max-h-48");
  });
});

const questionItem = (question: Record<string, unknown>) =>
  item({
    kind: "ask",
    action: "start_request",
    title: "Backend",
    payload: JSON.stringify({
      version: 1,
      prompt: "Which backend should the crew use?",
      options: [
        { id: "local", label: "Local model", description: "Runs on this Mac" },
        { id: "claude", label: "Claude" },
      ],
      multi_select: false,
      allow_free_text: false,
      ...question,
    }),
  });

describe("mailbox question", () => {
  it("sends a single choice on click and hides the raw payload", async () => {
    const answer = vi.fn().mockResolvedValue(undefined);
    const ask = questionItem({});
    renderMailbox(shellWith([ask], answer));
    expect(screen.getByText("Which backend should the crew use?")).toBeVisible();
    expect(screen.queryByText(/"version"/)).toBeNull();
    expect(screen.getByText("Runs on this Mac")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Claude" }));
    expect(answer).toHaveBeenCalledWith(ask, {
      option_ids: ["claude"],
      free_text: null,
    });
    // a sent answer may wait behind the asking turn; it cannot be sent twice
    expect(await screen.findByText("Answer sent to the agent.")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Claude" })).toBeNull();
  });

  it("toggles several choices and sends them with an Other note", () => {
    const answer = vi.fn().mockResolvedValue(undefined);
    const ask = questionItem({ multi_select: true, allow_free_text: true });
    renderMailbox(shellWith([ask], answer));
    const send = screen.getByRole("button", { name: "Send" });
    expect(send).toBeDisabled();
    const local = screen.getByRole("button", { name: "Local model" });
    fireEvent.click(local);
    fireEvent.click(screen.getByRole("button", { name: "Claude" }));
    fireEvent.click(local);
    expect(local).toHaveAttribute("aria-pressed", "false");
    fireEvent.change(screen.getByLabelText("Other"), {
      target: { value: "and a fallback" },
    });
    fireEvent.click(send);
    expect(answer).toHaveBeenCalledWith(ask, {
      option_ids: ["claude"],
      free_text: "and a fallback",
    });
  });

  it("sends a free-text-only answer", () => {
    const answer = vi.fn().mockResolvedValue(undefined);
    const ask = questionItem({ allow_free_text: true });
    renderMailbox(shellWith([ask], answer));
    fireEvent.change(screen.getByLabelText("Other"), { target: { value: "Ollama" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(answer).toHaveBeenCalledWith(ask, { option_ids: [], free_text: "Ollama" });
  });

  it("keeps the generic view for a malformed question", () => {
    for (const bad of [
      {
        options: [
          { id: "a", label: "A", description: { text: "x" } },
          { id: "b", label: "B" },
        ],
      },
      {
        options: [
          { id: "a", label: "A" },
          { id: "a", label: "B" },
        ],
      },
      { multi_select: "yes" },
      { extra: true },
      {
        options: [
          { id: "a", label: "A", icon: "x" },
          { id: "b", label: "B" },
        ],
      },
      { prompt: " " },
    ]) {
      const { unmount } = renderMailbox(shellWith([questionItem(bad)]));
      expect(screen.queryByTestId("mailbox-question")).toBeNull();
      unmount();
    }
  });

  it("keeps the generic view for a payload that is not a question", () => {
    renderMailbox(
      shellWith([item({ kind: "ask", action: "start_request", payload: '{"pr":42}' })]),
    );
    expect(screen.queryByTestId("mailbox-question")).toBeNull();
    expect(
      screen.getByTestId("mailbox-item-body").querySelector("pre"),
    ).toHaveTextContent('"pr": 42');
  });
});
