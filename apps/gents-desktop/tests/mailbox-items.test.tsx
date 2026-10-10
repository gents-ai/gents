import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { node, testApp, withApp } from "./app-fixture";

/* the hover cards are not what these cases are about */
vi.mock("../src/ui/screens/HoverCards", () => ({
  NodeHoverCard: ({ children }: { children: ReactElement }) => children,
  AgentHoverCard: ({ children }: { children: ReactElement }) => children,
}));

const item = (over: Partial<MailboxItemView> = {}): MailboxItemView => ({
  itemId: "item-1",
  itemKey: "key-1",
  requesterDid: "did:key:person",
  /* the node that lists the item: the bridge lists a node's own items */
  nodeDid: "did:key:node",
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
  targetNodeDid: "did:key:worker",
  targetAgentId: "engineer",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: new Date(Date.now() - 5 * 60_000).toISOString(),
  ...over,
});

/* a mailbox holding `items`, whose answers reach the bridge as `send` */
const mailboxWith = (
  items: MailboxItemView[],
  send: ReturnType<typeof vi.fn> = vi.fn().mockResolvedValue({}),
) => testApp({ api: { sendChatMessage: send }, deployments: [deploymentWith(items)] });
const deploymentWith = (items: MailboxItemView[]) =>
  node({
    nodeDid: "did:key:node",
    mailboxItems: items,
    agents: [{ agentId: "engineer", displayName: "Engineer" }],
    sessions: [{ sessionId: "session-1", title: "Mailbox cleanup" }],
  });

const renderMailbox = (app: DesktopApp) =>
  render(<MailboxScreen />, { wrapper: withApp(app) });

afterEach(() => vi.unstubAllGlobals());

describe("mailbox item", () => {
  it("shows the title, sender, session, time, kind and status", () => {
    renderMailbox(mailboxWith([item()]));
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
      '["event","did:key:worker","did:key:person","engineer","request-1"]';
    renderMailbox(mailboxWith([item({ sourceKind: "agent", sourceId })]));
    expect(screen.queryByText(sourceId, { exact: false })).toBeNull();
    expect(screen.queryByText(/did:key:/)).toBeNull();
  });

  it("renders the summary as markdown and keeps its line breaks", () => {
    const summary = "First line\nsecond line\n\n- one\n- two\n\n**bold**";
    renderMailbox(mailboxWith([item({ summary })]));
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
      mailboxWith([
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
    renderMailbox(mailboxWith([item({ summary })]));
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
    renderMailbox(mailboxWith([item({ summary: "short" })]));
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
    renderMailbox(mailboxWith([ask], answer));
    expect(screen.getByText("Which backend should the crew use?")).toBeVisible();
    expect(screen.queryByText(/"version"/)).toBeNull();
    expect(screen.getByText("Runs on this Mac")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Claude" }));
    expect(answer).toHaveBeenCalledWith(
      expect.objectContaining({
        causedBySourceDocId: ask.itemId,
        answer: {
          option_ids: ["claude"],
          free_text: null,
        },
      }),
    );
    // a sent answer may wait behind the asking turn; it cannot be sent twice
    expect(await screen.findByText("Answer sent to the agent.")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Claude" })).toBeNull();
  });

  it("toggles several choices and sends them with an Other note", () => {
    const answer = vi.fn().mockResolvedValue(undefined);
    const ask = questionItem({ multi_select: true, allow_free_text: true });
    renderMailbox(mailboxWith([ask], answer));
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
    expect(answer).toHaveBeenCalledWith(
      expect.objectContaining({
        causedBySourceDocId: ask.itemId,
        answer: {
          option_ids: ["claude"],
          free_text: "and a fallback",
        },
      }),
    );
  });

  it("sends a free-text-only answer", () => {
    const answer = vi.fn().mockResolvedValue(undefined);
    const ask = questionItem({ allow_free_text: true });
    renderMailbox(mailboxWith([ask], answer));
    fireEvent.change(screen.getByLabelText("Other"), { target: { value: "Ollama" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(answer).toHaveBeenCalledWith(
      expect.objectContaining({
        causedBySourceDocId: ask.itemId,
        answer: { option_ids: [], free_text: "Ollama" },
      }),
    );
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
      const { unmount } = renderMailbox(mailboxWith([questionItem(bad)]));
      expect(screen.queryByTestId("mailbox-question")).toBeNull();
      unmount();
    }
  });

  it("keeps the generic view for a payload that is not a question", () => {
    renderMailbox(
      mailboxWith([
        item({ kind: "ask", action: "start_request", payload: '{"pr":42}' }),
      ]),
    );
    expect(screen.queryByTestId("mailbox-question")).toBeNull();
    expect(
      screen.getByTestId("mailbox-item-body").querySelector("pre"),
    ).toHaveTextContent('"pr": 42');
  });
});

describe("a mailbox opened from a node", () => {
  it("narrows to the node the route names, and keeps that choice", () => {
    const nodeWith = (nodeDid: string, title: string) =>
      node({
        nodeDid,
        mailboxItems: [item({ itemId: `${nodeDid}-item`, nodeDid, title })],
      });
    const app = testApp({
      deployments: [nodeWith("did:key:a", "From A"), nodeWith("did:key:b", "From B")],
    });
    const view = render(<MailboxScreen nodeDid="did:key:b" />, {
      wrapper: withApp(app),
    });
    expect(screen.getByRole("heading", { name: "From B" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "From A" })).not.toBeInTheDocument();
    /* back on the mailbox without a node, the narrowing it was left with holds */
    view.rerender(<MailboxScreen />);
    expect(screen.queryByRole("heading", { name: "From A" })).not.toBeInTheDocument();
  });

  it("keeps the filters when the routed node has nothing open, so it can be cleared", () => {
    const app = testApp({
      deployments: [
        node({
          nodeDid: "did:key:a",
          mailboxItems: [
            item({ itemId: "a-item", nodeDid: "did:key:a", title: "From A" }),
          ],
        }),
        node({ nodeDid: "did:key:quiet", mailboxItems: [] }),
      ],
    });
    render(<MailboxScreen nodeDid="did:key:quiet" />, { wrapper: withApp(app) });
    expect(screen.queryByRole("heading", { name: "From A" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(screen.getByRole("heading", { name: "From A" })).toBeInTheDocument();
  });
});

describe("dismissing one item from its card", () => {
  it("reports a failure once and leaves no rejection unhandled", async () => {
    const reportFailure = vi.fn();
    const app = testApp({
      api: { dismissMailboxItem: vi.fn().mockRejectedValue(new Error("offline")) },
      deployments: [deploymentWith([item()])],
      reportFailure,
    });
    render(<MailboxScreen />, { wrapper: withApp(app) });
    const card = screen.getByTestId("mailbox-item");
    fireEvent.click(within(card).getByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(reportFailure).toHaveBeenCalledOnce());
    expect(reportFailure.mock.calls[0]![0]).toContain("offline");
  });
});
