import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  RequestAttachment,
} from "@source-inc/gents-desktop-client";

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));

import { TranscriptPanel } from "../src/ui/screens/Transcript";
import { SelectedPendingTray } from "../src/ui/screens/PendingTray";
import { ChatDeliveryFeedback } from "../src/ui/screens/session/ChatDeliveryFeedback";
import { NO_PARENT } from "../src/ui/screens/parentWork";
import { NO_WORKERS } from "../src/ui/screens/workers";
import { useSelectedSession } from "../src/ui/hooks/useSelectedSession";
import { createChatDelivery } from "../src/hooks/chatDelivery";
import {
  chatDraftOf,
  loadChatDrafts,
  saveChatDraft,
  type ChatDraft,
} from "../src/hooks/chatDrafts";
import { attachmentMetadata } from "../src/hooks/attachmentMetadata";
import { writeSession } from "../src/hooks/sessionStore";
import { sessionDraftKey } from "../src/hooks/shellView";
import { node, testApp, withApp } from "./app-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { acceptedChat } from "./chat-delivery-fixture";
import { deferred } from "./deferred";
import { memorySendAttachments } from "./send-attachments-fixture";
import { sessionSnapshot, userMessage } from "./timeline-fixture";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());

const NODE = node().nodeDid;
const SESSION = "session-open";

function selectedTranscript() {
  return function Selected() {
    return (
      <>
        <TranscriptPanel
          inFlight={false}
          scroller={null}
          workers={NO_WORKERS}
          parentWork={NO_PARENT}
          workerActions={{ interrupt: () => {} }}
          session={useSelectedSession()}
        />
        <div data-testid="pending-tray">
          <SelectedPendingTray />
        </div>
        <div data-testid="composer-feedback">
          <ChatDeliveryFeedback />
        </div>
      </>
    );
  };
}

function fixture({
  api = {},
  sendAttachments = memorySendAttachments(),
  session = sessionSnapshot({ sessionId: SESSION, nodeDid: NODE }),
}: {
  api?: object;
  sendAttachments?: ReturnType<typeof memorySendAttachments>;
  session?: DesktopSessionSnapshot;
} = {}) {
  const app = testApp({
    sendAttachments,
    api: { fetchSessionSnapshot: vi.fn().mockResolvedValue(null), ...api },
    deployments: [node()],
    session,
  });
  app.stores.client.setState((state) => ({
    snapshot: {
      ...state.snapshot!,
      client: { ...state.snapshot!.client!, clientDid: "client" },
    } as DesktopClientSnapshot,
  }));
  const Selected = selectedTranscript();
  const view = render(<Selected />, { wrapper: withApp(app) });
  return { app, view };
}

function request(overrides: Partial<ChatSendRequest> = {}): ChatSendRequest {
  return {
    clientDid: "client",
    nodeDid: NODE,
    requestId: "sent-request",
    sessionId: SESSION,
    agentDid: "default",
    content: "Sent from here",
    cwd: null,
    ...overrides,
  };
}

function keep(
  app: ReturnType<typeof fixture>["app"],
  input: ChatSendRequest,
  {
    selectedSessionId = input.sessionId ?? null,
    queued = false,
    failed = false,
    mailbox,
  }: {
    selectedSessionId?: string | null;
    queued?: boolean;
    failed?: boolean;
    mailbox?: ChatDraft["mailbox"];
  } = {},
) {
  const draft = chatDraftOf(input, selectedSessionId, queued, mailbox);
  act(() =>
    saveChatDraft(
      app.stores.chat,
      failed
        ? {
            ...draft,
            failure: { message: "The runtime connection was lost", error: null },
          }
        : { ...draft, pendingUntil: Date.now() + 60_000 },
    ),
  );
}

const rowsOf = (requestId: string) =>
  document.querySelectorAll(`[data-timeline-key="turn:${requestId}"]`);
const transcript = () => screen.getByTestId("transcript-panel");
const feedback = () => screen.getByTestId("composer-feedback");

describe("a send drawn in its transcript", () => {
  it("shows a pending send as its own row at once, marked as sending, and not beside the composer", () => {
    const { app } = fixture();
    keep(app, request());
    const row = rowsOf("sent-request");
    expect(row).toHaveLength(1);
    expect(row[0]).toHaveTextContent("Sent from here");
    expect(within(row[0] as HTMLElement).getByRole("status")).toHaveTextContent(
      "Sending…",
    );
    expect(
      within(feedback()).queryByRole("status", { name: "Saved message" }),
    ).toBeNull();
  });

  it("keeps the same row, drawn once, when the session's row for it arrives", () => {
    const { app } = fixture();
    keep(app, request());
    const before = rowsOf("sent-request")[0];
    act(() =>
      writeSession(
        app.stores.session,
        sessionSnapshot({
          sessionId: SESSION,
          nodeDid: NODE,
          latestRequestId: "sent-request",
          timelineItems: [
            userMessage({
              itemKey: "u-sent",
              requestId: "sent-request",
              sequence: 0,
              content: "Sent from here",
            }),
          ],
        }),
      ),
    );
    const after = rowsOf("sent-request");
    expect(after).toHaveLength(1);
    expect(after[0]).toBe(before);
    expect(within(transcript()).getAllByText("Sent from here")).toHaveLength(1);
    expect(within(after[0] as HTMLElement).queryByText("Sending…")).toBeNull();
  });

  it("draws a resend under the same identity once while the session holds it", () => {
    const { app } = fixture({
      session: sessionSnapshot({
        sessionId: SESSION,
        nodeDid: NODE,
        latestRequestId: "sent-request",
        timelineItems: [
          userMessage({
            itemKey: "u-sent",
            requestId: "sent-request",
            sequence: 0,
            content: "Sent from here",
          }),
        ],
      }),
    });
    keep(app, request());
    expect(rowsOf("sent-request")).toHaveLength(1);
    expect(within(transcript()).getAllByText("Sent from here")).toHaveLength(1);
    expect(screen.queryByText("Sending…")).toBeNull();
  });

  it("shows a failed send in its row with its cause, Retry and Discard", async () => {
    const send = vi.fn(async (input: ChatSendRequest) => acceptedChat(input));
    const { app } = fixture({ api: { sendChatMessage: send } });
    keep(app, request(), { failed: true });
    const row = rowsOf("sent-request")[0] as HTMLElement;
    expect(row).toHaveTextContent("Sent from here");
    within(row).getByText("The message wasn't sent");
    within(row).getByText(/The runtime connection was lost/);
    within(row).getByRole("button", { name: "Retry message" });
    fireEvent.click(within(row).getByRole("button", { name: "Discard message" }));
    await waitFor(() => expect(rowsOf("sent-request")).toHaveLength(0));
    expect(send).not.toHaveBeenCalled();
    expect(app.drafts.getState().drafts[sessionDraftKey(NODE, SESSION)]).toBe(
      "Sent from here",
    );
  });

  it("retries a failed send from its row under the same identity", async () => {
    const send = vi.fn(async (input: ChatSendRequest) => acceptedChat(input));
    const { app } = fixture({ api: { sendChatMessage: send } });
    keep(app, request(), { failed: true });
    fireEvent.click(screen.getByRole("button", { name: "Retry message" }));
    await waitFor(() => expect(loadChatDrafts().chatDrafts).toEqual([]));
    expect(send).toHaveBeenCalledWith(request());
    expect(screen.queryByText("The message wasn't sent")).toBeNull();
  });
});

describe("a send drawn only in the session it was sent to", () => {
  it("draws nothing in another session, another node's session of that id, or for another client", () => {
    const { app } = fixture();
    keep(app, request({ requestId: "elsewhere", sessionId: "another-session" }));
    keep(app, request({ requestId: "other-node", nodeDid: "did:key:other" }));
    keep(app, request({ requestId: "other-client", clientDid: "someone-else" }));
    for (const id of ["elsewhere", "other-node", "other-client"])
      expect(rowsOf(id)).toHaveLength(0);
    expect(within(transcript()).queryByText("Sent from here")).toBeNull();
  });

  it("leaves a new session's first message and an answer by the composer", () => {
    const { app } = fixture();
    keep(app, request({ requestId: "first", content: "First message" }), {
      selectedSessionId: null,
    });
    keep(
      app,
      request({ requestId: "answer", content: "", causedBySourceDocId: "item" }),
      { mailbox: { nodeDid: NODE, itemId: "item", summary: "An answer" } },
    );
    for (const id of ["first", "answer"]) expect(rowsOf(id)).toHaveLength(0);
    expect(feedback()).toHaveTextContent("Sending the message…");
    expect(feedback()).toHaveTextContent("First message");
    expect(feedback()).toHaveTextContent("An answer");
  });
});

describe("a send queued behind a running turn", () => {
  const queuedIn = (ids: string[]) =>
    sessionSnapshot({
      sessionId: SESSION,
      nodeDid: NODE,
      turnState: "running",
      queuedTurns: ids.map((requestId) => ({
        requestId,
        content: "Queued one",
        selectedSkillIds: [],
        lifecycleState: "pending",
        foldedIntoRequestId: null,
        origin: null,
        createdAt: null,
      })),
    });

  it("shows no card while pending, whether or not the tray lists it yet", () => {
    const { app } = fixture({ session: queuedIn([]) });
    keep(app, request({ requestId: "queued", content: "Queued one" }), {
      queued: true,
    });
    expect(
      within(feedback()).queryByRole("status", { name: "Saved message" }),
    ).toBeNull();
    act(() => writeSession(app.stores.session, queuedIn(["queued"])));
    expect(screen.getByTestId("pending-tray")).toHaveTextContent("Queued one");
    expect(
      within(feedback()).queryByRole("status", { name: "Saved message" }),
    ).toBeNull();
    expect(rowsOf("queued")).toHaveLength(0);
  });

  it("hides the card of a pending send the session holds, though it was not queued here", () => {
    const { app } = fixture({ session: queuedIn(["sent-request"]) });
    keep(app, request());
    expect(
      within(feedback()).queryByRole("status", { name: "Saved message" }),
    ).toBeNull();
    expect(screen.queryByText("Sending the message…")).toBeNull();
  });

  it("shows the card with Retry and Discard once when it fails", () => {
    const { app } = fixture({ session: queuedIn([]) });
    keep(app, request({ requestId: "queued", content: "Queued one" }), {
      queued: true,
      failed: true,
    });
    expect(screen.getAllByRole("button", { name: "Retry message" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Discard message" })).toHaveLength(1);
    within(feedback()).getByText("The message wasn't sent");
    within(feedback()).getByRole("button", { name: "Retry message" });
  });
});

describe("a send drawn only once it is saved", () => {
  const notes: RequestAttachment = {
    name: "notes.txt",
    mediaType: "text/plain",
    data: btoa("notes"),
  };

  function delivering(put: () => Promise<void>) {
    const kept = memorySendAttachments();
    const store = {
      ...kept,
      put: vi.fn(async (key: string, data: string[]) => {
        await put();
        await kept.put(key, data);
      }),
    };
    const reply = deferred<ReturnType<typeof acceptedChat>>();
    const send = vi.fn((_: ChatSendRequest) => reply.promise);
    const f = fixture({ sendAttachments: store, api: { sendChatMessage: send } });
    const delivery = createChatDelivery(
      { sendChatMessage: send } as unknown as DesktopApiAdapter,
      f.app.stores,
      store,
    );
    const handedOver = vi.fn();
    const sent = delivery.submit(
      { ...request(), requestId: undefined, attachments: [notes] },
      SESSION,
      false,
      async () => true,
      undefined,
      handedOver,
    );
    return { ...f, sent, send, reply, handedOver };
  }

  it("draws nothing while its attachments are stored, then the send with them", async () => {
    const stored = deferred<void>();
    const f = delivering(() => stored.promise);
    await waitFor(() => expect(loadChatDrafts().chatDrafts).toHaveLength(1));
    const [draft] = loadChatDrafts().chatDrafts;
    expect(rowsOf(draft.requestId)).toHaveLength(0);
    expect(
      within(feedback()).queryByRole("status", { name: "Saved message" }),
    ).toBeNull();
    await act(async () => stored.resolve());
    expect(f.handedOver).toHaveBeenCalledTimes(1);
    const row = rowsOf(draft.requestId);
    expect(row).toHaveLength(1);
    expect(row[0]).toHaveTextContent("Sent from here");
    expect(row[0]).toHaveTextContent((await attachmentMetadata(notes)).name);
    await act(async () => f.reply.resolve(acceptedChat(f.send.mock.calls[0][0])));
    await f.sent;
  });

  it("never draws a send whose attachments could not be stored", async () => {
    const refused = deferred<void>();
    const f = delivering(() => refused.promise);
    await waitFor(() => expect(loadChatDrafts().chatDrafts).toHaveLength(1));
    const [draft] = loadChatDrafts().chatDrafts;
    expect(rowsOf(draft.requestId)).toHaveLength(0);
    await act(async () => {
      refused.reject(new Error("The disk is full"));
      await f.sent.catch(() => {});
    });
    await expect(f.sent).rejects.toThrow(/attachments could not be saved/);
    expect(loadChatDrafts().chatDrafts).toEqual([]);
    expect(rowsOf(draft.requestId)).toHaveLength(0);
    expect(f.handedOver).not.toHaveBeenCalled();
    expect(f.send).not.toHaveBeenCalled();
    expect(f.app.stores.chat.getState().storingSend).toBeNull();
  });
});
