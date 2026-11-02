import { acceptedChatReply } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import type { ReactElement } from "react";
import type { Route } from "../src/ui/lib/router";
import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";

import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { MailboxStack, openInStack } from "../src/ui/screens/MailboxStack";
import { mailboxSeen } from "../src/ui/app/mailboxSeen";
import { listViews } from "../src/ui/app/listViews";
import { node, testApp, withApp } from "./app-fixture";

vi.mock("../src/ui/screens/HoverCards", () => ({
  NodeHoverCard: ({ children }: { children: ReactElement }) => children,
  AgentHoverCard: ({ children }: { children: ReactElement }) => children,
}));

const item = (over: Partial<MailboxItemView> = {}): MailboxItemView => ({
  itemId: "item-1",
  itemKey: "key-1",
  requesterDid: "did:key:person",
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
  causeDocId: null,
  targetNodeDid: "did:key:worker",
  targetAgentId: "engineer",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: new Date(Date.now() - 5 * 60_000).toISOString(),
  ...over,
});

const mailboxWith = (
  items: MailboxItemView[],
  send: ReturnType<typeof vi.fn> = vi.fn(acceptedChatReply),
) =>
  testApp({
    api: {
      sendChatMessage: send,
      acknowledgeChatSubmission: vi.fn().mockResolvedValue(undefined),
    },
    deployments: [deploymentWith(items)],
  });
const deploymentWith = (items: MailboxItemView[]) =>
  node({
    nodeDid: "did:key:node",
    mailboxItems: items,
    agents: [{ agentId: "engineer", displayName: "Engineer" }],
    sessions: [{ sessionId: "session-1", title: "Mailbox cleanup" }],
  });

/* the mailbox page and the app-wide dialog its rows open, as the app draws
   them; `route` is the page shown, for the dialog opened over other pages */
function Page({ route = { name: "mailbox" } }: { route?: Route }) {
  return (
    <>
      {route.name === "mailbox" && <MailboxScreen />}
      <MailboxStack route={route} />
    </>
  );
}

const renderMailbox = (app: DesktopApp, route?: Route) =>
  render(<Page route={route} />, { wrapper: withApp(app) });

const rowOf = (title: string) =>
  within(screen.getByTestId("mailbox-list"))
    .getByText(title)
    .closest('[data-testid="mailbox-item"]') as HTMLElement;

/* the row's own button opens it; the row's other button dismisses it */
const open = (title: string) =>
  fireEvent.click(within(rowOf(title)).getAllByRole("button")[0]!);

const dialog = () => screen.getByTestId("mailbox-dialog");

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
const resetOpen = () => listViews.setMailboxOpen(null);
afterEach(() => vi.unstubAllGlobals());

describe("the mailbox list", () => {
  it("lists an item with its kind, sender, session and time", () => {
    renderMailbox(mailboxWith([item()]));
    const row = rowOf("Refactor landed");
    expect(within(row).getByText("Finished")).toBeVisible();
    expect(within(row).getByText("Engineer")).toBeVisible();
    expect(within(row).getByText("Mailbox cleanup")).toBeVisible();
    const time = within(row).getByText(/^\d+m$/);
    expect(time.tagName).toBe("TIME");
    expect(time).toHaveAttribute("title");
  });

  it("names the source by agent, session and time, never its raw identity", () => {
    const sourceId =
      '["event","did:key:worker","did:key:person","engineer","request-1"]';
    renderMailbox(mailboxWith([item({ sourceKind: "agent", sourceId })]));
    open("Refactor landed");
    expect(screen.queryByText(sourceId, { exact: false })).toBeNull();
    expect(screen.queryByText(/did:key:/)).toBeNull();
  });

  it("splits the list into tabs by what each item needs, with their counts", () => {
    renderMailbox(
      mailboxWith([
        item({ itemId: "a", kind: "failed", title: "Build failed" }),
        item({ itemId: "b", kind: "finished", title: "Refactor landed" }),
        item({ itemId: "c", kind: "finished", title: "Docs landed" }),
      ]),
    );
    expect(screen.getByRole("tab", { name: "All 3" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByRole("tab", { name: "Went wrong 1" })).toBeVisible();
    fireEvent.click(screen.getByRole("tab", { name: "Done 2" }));
    const list = screen.getByTestId("mailbox-list");
    expect(within(list).queryByText("Build failed")).toBeNull();
    expect(within(list).getByText("Refactor landed")).toBeVisible();
    expect(within(list).getByText("Docs landed")).toBeVisible();
  });

  it("dismisses an item from its row without opening it", () => {
    const dismiss = vi.fn().mockResolvedValue(undefined);
    const app = testApp({
      api: { dismissMailboxItem: dismiss },
      deployments: [deploymentWith([item()])],
    });
    renderMailbox(app);
    fireEvent.click(screen.getByRole("button", { name: "Dismiss Refactor landed" }));
    expect(dismiss).toHaveBeenCalledExactlyOnceWith("item-1");
    expect(screen.queryByTestId("mailbox-dialog")).toBeNull();
  });

  it("reports a failed dismissal once and leaves no rejection unhandled", async () => {
    const reportFailure = vi.fn();
    const app = testApp({
      api: { dismissMailboxItem: vi.fn().mockRejectedValue(new Error("offline")) },
      deployments: [deploymentWith([item()])],
      reportFailure,
    });
    renderMailbox(app);
    fireEvent.click(screen.getByRole("button", { name: "Dismiss Refactor landed" }));
    await waitFor(() => expect(reportFailure).toHaveBeenCalledOnce());
    expect(reportFailure.mock.calls[0]![0]).toContain("offline");
  });

  it("marks an item new until it is opened here", () => {
    mailboxSeen.start(new Date(Date.now() - 60 * 60_000).toISOString());
    renderMailbox(mailboxWith([item()]));
    expect(
      within(rowOf("Refactor landed")).getByRole("img", { name: "New" }),
    ).toBeVisible();
    open("Refactor landed");
    fireEvent.keyDown(dialog(), { key: "Escape" });
    expect(
      within(rowOf("Refactor landed")).queryByRole("img", { name: "New" }),
    ).toBeNull();
  });

  it("counts nothing that came before this device kept track as new", () => {
    mailboxSeen.start(new Date().toISOString());
    renderMailbox(mailboxWith([item()]));
    expect(
      within(rowOf("Refactor landed")).queryByRole("img", { name: "New" }),
    ).toBeNull();
  });
});

describe("an opened item", () => {
  it("shows its kind, sender, session, time and summary", () => {
    renderMailbox(mailboxWith([item({ summary: "Merged into main." })]));
    open("Refactor landed");
    expect(
      within(dialog()).getByRole("heading", { name: "Refactor landed" }),
    ).toBeVisible();
    const meta = within(dialog()).getByTestId("mailbox-item-meta");
    expect(within(meta).getByText("Finished")).toBeVisible();
    expect(meta).toHaveTextContent("Engineer · in Mailbox cleanup");
    expect(within(meta).getByRole("link", { name: "Mailbox cleanup" })).toHaveAttribute(
      "href",
    );
    expect(within(meta).getByText(/^\d+m$/).tagName).toBe("TIME");
    expect(within(dialog()).getByText("Merged into main.")).toBeVisible();
  });

  it("renders the summary as markdown and keeps its line breaks", () => {
    const summary = "First line\nsecond line\n\n- one\n- two\n\n**bold**";
    renderMailbox(mailboxWith([item({ summary })]));
    open("Refactor landed");
    const body = within(dialog()).getByTestId("mailbox-item-body");
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
    open("json");
    const json = within(dialog()).getByTestId("mailbox-item-body");
    expect(json.querySelector("pre")).toHaveTextContent('"pr": 42');
    fireEvent.click(within(dialog()).getByRole("button", { name: "Next item" }));
    const text = within(dialog()).getByTestId("mailbox-item-body");
    expect(text.querySelector("h2")).toHaveTextContent("Next");
    expect(text.querySelector("ol li")).toHaveTextContent("review");
  });

  it("moves through the list with its arrows and the arrow keys", () => {
    renderMailbox(
      mailboxWith([
        item({ itemId: "a", title: "First", createdAt: new Date().toISOString() }),
        item({ itemId: "b", title: "Second" }),
        item({ itemId: "c", title: "Third", createdAt: "2020-01-01T00:00:00Z" }),
      ]),
    );
    open("First");
    expect(within(dialog()).getByText("1 of 3")).toBeVisible();
    expect(
      within(dialog()).getByRole("button", { name: "Previous item" }),
    ).toBeDisabled();
    fireEvent.keyDown(dialog(), { key: "ArrowRight" });
    expect(within(dialog()).getByRole("heading", { name: "Second" })).toBeVisible();
    fireEvent.click(within(dialog()).getByRole("button", { name: "Next item" }));
    expect(within(dialog()).getByRole("heading", { name: "Third" })).toBeVisible();
    expect(within(dialog()).getByRole("button", { name: "Next item" })).toBeDisabled();
    fireEvent.keyDown(dialog(), { key: "ArrowLeft" });
    expect(within(dialog()).getByText("2 of 3")).toBeVisible();
  });

  it("moves on to the next item once one is dismissed, and closes after the last", async () => {
    const dismiss = vi.fn().mockResolvedValue(undefined);
    const app = testApp({
      api: { dismissMailboxItem: dismiss },
      deployments: [
        deploymentWith([
          item({ itemId: "a", title: "First", createdAt: new Date().toISOString() }),
          item({ itemId: "b", title: "Second" }),
        ]),
      ],
    });
    renderMailbox(app);
    open("First");
    fireEvent.click(within(dialog()).getByRole("button", { name: "Dismiss" }));
    expect(
      await within(dialog()).findByRole("heading", { name: "Second" }),
    ).toBeVisible();
    fireEvent.click(within(dialog()).getByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(screen.queryByTestId("mailbox-dialog")).toBeNull());
    expect(dismiss.mock.calls).toEqual([["a"], ["b"]]);
  });

  it("waits while another page shows, and opens again on coming back", () => {
    const app = mailboxWith([item({ itemId: "a", title: "First" })]);
    const view = renderMailbox(app);
    open("First");
    view.rerender(<Page route={{ name: "session", sessionId: "session-1" }} />);
    expect(screen.queryByTestId("mailbox-dialog")).toBeNull();
    view.rerender(<Page />);
    expect(within(dialog()).getByRole("heading", { name: "First" })).toBeVisible();
  });

  it("goes through every item away from the Mailbox, whatever its tab, and links to it", () => {
    listViews.setMailboxTab("done");
    const app = mailboxWith([
      item({
        itemId: "a",
        title: "Flagged",
        kind: "flag",
        createdAt: new Date().toISOString(),
      }),
      item({ itemId: "b", title: "Landed", kind: "finished" }),
    ]);
    renderMailbox(app, { name: "session", sessionId: "session-1" });
    fireEvent.keyDown(window, {
      key: "M",
      code: "KeyM",
      metaKey: true,
      shiftKey: true,
    });
    expect(within(dialog()).getByText("1 of 2")).toBeVisible();
    expect(within(dialog()).getByRole("heading", { name: "Flagged" })).toBeVisible();
    expect(within(dialog()).getByRole("link", { name: "Mailbox" })).toHaveAttribute(
      "href",
      expect.stringMatching(/\/mailbox$/),
    );
    listViews.setMailboxTab("all");
  });

  it("keeps to the Mailbox's tab on the Mailbox, with no link to itself", () => {
    listViews.setMailboxTab("done");
    renderMailbox(
      mailboxWith([
        item({ itemId: "a", title: "Flagged", kind: "flag" }),
        item({ itemId: "b", title: "Landed", kind: "finished" }),
      ]),
    );
    open("Landed");
    expect(within(dialog()).queryByText(/of 2/)).toBeNull();
    expect(within(dialog()).queryByRole("link", { name: "Mailbox" })).toBeNull();
    listViews.setMailboxTab("all");
  });

  it("opens an item from outside the page's list over every item, the page's tab left as it was", () => {
    listViews.setMailboxTab("done");
    const flagged = item({ itemId: "a", title: "Flagged", kind: "flag" });
    renderMailbox(
      mailboxWith([flagged, item({ itemId: "b", title: "Landed", kind: "finished" })]),
    );
    /* as the side nav's mailbox section and the search palette do */
    act(() => openInStack(flagged));
    expect(within(dialog()).getByRole("heading", { name: "Flagged" })).toBeVisible();
    expect(within(dialog()).getByText("1 of 2")).toBeVisible();
    /* the page behind still lists its Done tab alone */
    const list = screen.getByTestId("mailbox-list");
    expect(within(list).getByText("Landed")).toBeInTheDocument();
    expect(within(list).queryByText("Flagged")).toBeNull();
    listViews.setMailboxTab("all");
  });

  it("stays with the node it was opened on", () => {
    const nodeWith = (suffix: string) =>
      node({
        nodeDid: `did:key:${suffix}`,
        mailboxItems: [
          item({ itemId: "shared", nodeDid: `did:key:${suffix}`, title: suffix }),
        ],
      });
    const app = testApp({
      deployments: [nodeWith("a"), nodeWith("b")],
      selection: { nodeDid: "did:key:a" },
    });
    renderMailbox(app);
    open("a");
    act(() => app.actions.selectNode("did:key:b"));
    expect(screen.queryByTestId("mailbox-dialog")).toBeNull();
    act(() => app.actions.selectNode("did:key:a"));
    expect(within(dialog()).getByRole("heading", { name: "a" })).toBeVisible();
  });

  it("opens over another page with ⌘⇧M, and closes with it again", () => {
    renderMailbox(mailboxWith([item()]), { name: "sessions" });
    expect(screen.queryByTestId("mailbox-dialog")).toBeNull();
    fireEvent.keyDown(window, {
      key: "M",
      code: "KeyM",
      metaKey: true,
      shiftKey: true,
    });
    expect(
      within(dialog()).getByRole("heading", { name: "Refactor landed" }),
    ).toBeVisible();
    fireEvent.keyDown(window, {
      key: "M",
      code: "KeyM",
      metaKey: true,
      shiftKey: true,
    });
    expect(screen.queryByTestId("mailbox-dialog")).toBeNull();
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

describe("a question in the mailbox", () => {
  it.each([2, 3])(
    "skips a version %i optional question with its observed source",
    async (version) => {
      const answer = vi.fn(acceptedChatReply);
      const ask = {
        ...questionItem({ version, optional: true }),
        questionHash: "original-question",
      };
      renderMailbox(mailboxWith([ask], answer));
      expect(screen.queryByRole("button", { name: "Dismiss Backend" })).toBeNull();
      open("Backend");
      expect(within(dialog()).queryByRole("button", { name: "Dismiss" })).toBeNull();
      fireEvent.click(within(dialog()).getByRole("button", { name: "Skip" }));
      expect(answer).toHaveBeenCalledWith(
        expect.objectContaining({
          causedBySourceDocId: ask.itemId,
          answer: {
            option_ids: [],
            free_text: null,
            skip: true,
            question_hash: "original-question",
          },
        }),
      );
      await waitFor(() => expect(screen.queryByTestId("mailbox-dialog")).toBeNull());
    },
  );

  it.each([2, 3])(
    "keeps version %i required free-text questions pending until an explicit answer",
    async (version) => {
      const answer = vi.fn(acceptedChatReply);
      const ask = {
        ...questionItem({ version, options: [], allow_free_text: true }),
        questionHash: "required-question",
      };
      renderMailbox(mailboxWith([ask], answer));
      open("Backend");
      expect(within(dialog()).queryByRole("button", { name: "Skip" })).toBeNull();
      expect(within(dialog()).queryByRole("button", { name: "Dismiss" })).toBeNull();
      expect(within(dialog()).getByRole("button", { name: "Send" })).toBeDisabled();
      fireEvent.change(within(dialog()).getByRole("textbox", { name: "Your answer" }), {
        target: { value: "Ship Friday" },
      });
      expect(answer).not.toHaveBeenCalled();
      fireEvent.click(within(dialog()).getByRole("button", { name: "Send" }));
      expect(answer).toHaveBeenCalledWith(
        expect.objectContaining({
          answer: {
            option_ids: [],
            free_text: "Ship Friday",
            question_hash: "required-question",
          },
        }),
      );
      await waitFor(() => expect(screen.queryByTestId("mailbox-dialog")).toBeNull());
    },
  );

  it.each([2, 3])(
    "refuses to invent a source observation for a version %i question",
    (version) => {
      const answer = vi.fn();
      renderMailbox(mailboxWith([questionItem({ version, optional: true })], answer));
      open("Backend");
      expect(within(dialog()).getByRole("status")).toHaveTextContent(
        "Reconnect to load this question",
      );
      expect(within(dialog()).getByRole("button", { name: "Skip" })).toBeDisabled();
      expect(
        within(dialog()).getByRole("radio", { name: /Local model/ }),
      ).toBeDisabled();
      expect(answer).not.toHaveBeenCalled();
    },
  );

  it("sends a single choice when it is picked, with the context the agent gave", async () => {
    const answer = vi.fn(acceptedChatReply);
    const ask = { ...questionItem({}), summary: "The crew needs a model tonight." };
    renderMailbox(mailboxWith([ask], answer));
    open("Backend");
    const panel = within(dialog()).getByTestId("question-panel");
    expect(within(panel).getByText("Which backend should the crew use?")).toBeVisible();
    expect(within(panel).getByText("The crew needs a model tonight.")).toBeVisible();
    expect(within(panel).getByRole("link", { name: "Mailbox cleanup" })).toBeVisible();
    expect(within(panel).queryByText(/"version"/)).toBeNull();
    expect(within(panel).getByText("Runs on this Mac")).toBeVisible();
    fireEvent.click(within(panel).getByRole("radio", { name: /Claude/ }));
    expect(answer).toHaveBeenCalledWith(
      expect.objectContaining({
        causedBySourceDocId: ask.itemId,
        answer: { option_ids: ["claude"], free_text: null },
      }),
    );
    await waitFor(() => expect(screen.queryByTestId("mailbox-dialog")).toBeNull());
  });

  it("toggles several choices and sends them with a written note", () => {
    const answer = vi.fn(acceptedChatReply);
    const ask = questionItem({ multi_select: true, allow_free_text: true });
    renderMailbox(mailboxWith([ask], answer));
    open("Backend");
    const send = within(dialog()).getByRole("button", { name: "Send" });
    expect(send).toBeDisabled();
    const local = within(dialog()).getByRole("checkbox", { name: /Local model/ });
    fireEvent.click(local);
    fireEvent.click(within(dialog()).getByRole("checkbox", { name: /Claude/ }));
    fireEvent.click(local);
    expect(local).toHaveAttribute("aria-checked", "false");
    fireEvent.change(
      within(dialog()).getByRole("textbox", { name: "Something else" }),
      {
        target: { value: "and a fallback" },
      },
    );
    fireEvent.click(send);
    expect(answer).toHaveBeenCalledWith(
      expect.objectContaining({
        causedBySourceDocId: ask.itemId,
        answer: { option_ids: ["claude"], free_text: "and a fallback" },
      }),
    );
  });

  it("sends a written answer instead of a choice", () => {
    const answer = vi.fn(acceptedChatReply);
    const ask = questionItem({ allow_free_text: true });
    renderMailbox(mailboxWith([ask], answer));
    open("Backend");
    fireEvent.change(
      within(dialog()).getByRole("textbox", { name: "Something else" }),
      {
        target: { value: "Ollama" },
      },
    );
    fireEvent.click(within(dialog()).getByRole("button", { name: "Send" }));
    expect(answer).toHaveBeenCalledWith(
      expect.objectContaining({
        causedBySourceDocId: ask.itemId,
        answer: { option_ids: [], free_text: "Ollama" },
      }),
    );
  });

  it("moves on with the arrow keys from a text-only question's empty field", () => {
    renderMailbox(
      mailboxWith([
        {
          ...questionItem({ version: 2, options: [], allow_free_text: true }),
          questionHash: "text-question",
          createdAt: new Date().toISOString(),
        },
        item({ itemId: "b", title: "Second" }),
      ]),
    );
    open("Backend");
    const field = within(dialog()).getByRole("textbox", { name: "Your answer" });
    expect(field).toHaveFocus();
    fireEvent.change(field, { target: { value: "Ship" } });
    fireEvent.keyDown(field, { key: "ArrowRight" });
    expect(within(dialog()).getByText("1 of 2")).toBeVisible();
    fireEvent.change(field, { target: { value: "" } });
    fireEvent.keyDown(field, { key: "ArrowRight" });
    expect(within(dialog()).getByRole("heading", { name: "Second" })).toBeVisible();
  });

  it("keeps the arrow keys working past a question onto an item without one", () => {
    renderMailbox(
      mailboxWith([
        { ...questionItem({}), createdAt: new Date().toISOString() },
        item({ itemId: "b", title: "Second" }),
        item({ itemId: "c", title: "Third", createdAt: "2020-01-01T00:00:00Z" }),
      ]),
    );
    open("Backend");
    /* the keys go where focus is, as a keyboard's do */
    const press = (key: string) => fireEvent.keyDown(document.activeElement!, { key });
    press("ArrowRight");
    expect(within(dialog()).getByRole("heading", { name: "Second" })).toBeVisible();
    expect(dialog()).toContainElement(document.activeElement as HTMLElement);
    press("ArrowRight");
    expect(within(dialog()).getByRole("heading", { name: "Third" })).toBeVisible();
  });

  it("shows a malformed question as its details", () => {
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
      open("Backend");
      expect(screen.queryByTestId("question-panel")).toBeNull();
      expect(within(dialog()).getByRole("heading", { name: "Backend" })).toBeVisible();
      unmount();
      act(() => resetOpen());
    }
  });

  it("shows a payload that is not a question as its details", () => {
    renderMailbox(
      mailboxWith([
        item({ kind: "ask", action: "start_request", payload: '{"pr":42}' }),
      ]),
    );
    open("Refactor landed");
    expect(screen.queryByTestId("question-panel")).toBeNull();
    expect(
      within(dialog()).getByTestId("mailbox-item-body").querySelector("pre"),
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
    act(() => app.actions.selectNode("did:key:b"));
    const view = renderMailbox(app);
    expect(screen.getByText("From B")).toBeInTheDocument();
    expect(screen.queryByText("From A")).not.toBeInTheDocument();
    view.rerender(<Page />);
    expect(screen.queryByText("From A")).not.toBeInTheDocument();
  });

  it("keeps an empty active node scoped until another node is selected", () => {
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
    act(() => app.actions.selectNode("did:key:quiet"));
    renderMailbox(app);
    expect(screen.queryByText("From A")).not.toBeInTheDocument();
    act(() => app.actions.selectNode("did:key:a"));
    expect(screen.getByText("From A")).toBeInTheDocument();
  });
});
