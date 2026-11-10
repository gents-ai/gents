import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  RequestAttachment,
} from "@source-inc/gents-desktop-client";
import { ChatDeliveryFeedback } from "../src/ui/screens/session/ChatDeliveryFeedback";
import { attachmentMetadata } from "../src/hooks/attachmentMetadata";
import { chatDraftKey, chatDraftOf, saveChatDraft } from "../src/hooks/chatDrafts";
import { acceptedChat } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { loadChatDrafts } from "../src/hooks/chatDrafts";
import { navigate } from "../src/ui/lib/router";
import { node, testApp, withApp } from "./app-fixture";
import { writeSession } from "../src/hooks/sessionStore";
import { sessionDraftKey } from "../src/hooks/shellView";
import { deferred } from "./deferred";
import { memorySendAttachments } from "./send-attachments-fixture";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";

vi.mock("@/lib/router", async (original) => ({
  ...(await original<typeof import("../src/ui/lib/router")>()),
  navigate: vi.fn(),
}));
beforeEach(() => {
  vi.mocked(navigate).mockClear();
  vi.stubGlobal("localStorage", goalDeliveryStorage());
});
afterEach(() => vi.unstubAllGlobals());

/* `sentFrom` is the session whose composer sent the message, which is the
   one shown; null is the new session composer */
async function fixture(
  sending = false,
  sentFrom: string | null = null,
  attachments: RequestAttachment[] = [],
) {
  const nodeDid = node().nodeDid;
  const request: ChatSendRequest = {
    clientDid: "client",
    nodeDid,
    requestId: "original-request",
    sessionId: sentFrom ?? "original-session",
    agentDid: "default",
    content: "Original message",
    cwd: "/original",
    ...(attachments.length ? { attachments } : {}),
  };
  const control = vi.fn(async (request: ChatSendRequest) => acceptedChat(request));
  const kept = memorySendAttachments();
  const app = testApp({
    sendAttachments: kept,
    api: {
      sendChatMessage: control,
      fetchSessionSnapshot: vi.fn().mockResolvedValue(null),
    },
    deployments: [node()],
    selection: { nodeDid, sessionId: sentFrom },
  });
  app.stores.client.setState((state) => ({
    snapshot: {
      ...state.snapshot!,
      client: { ...state.snapshot!.client!, clientDid: "client" },
    },
  }));
  app.drafts.setState({ drafts: { newer: "Keep this newer draft" } });
  const draft = chatDraftOf(
    request,
    sentFrom,
    false,
    undefined,
    await Promise.all(attachments.map(attachmentMetadata)),
  );
  saveChatDraft(
    app.stores.chat,
    sending
      ? { ...draft, pendingUntil: Date.now() + 60_000 }
      : {
          ...draft,
          failure: { message: "The runtime connection was lost", error: null },
        },
  );
  if (attachments.length)
    await kept.put(
      chatDraftKey(draft),
      attachments.map((attachment) => attachment.data),
    );
  const view = render(<ChatDeliveryFeedback />, { wrapper: withApp(app) });
  return { app, request, control, view };
}

it("shows a message as sending from the moment it is sent, with no actions", async () => {
  await fixture(true);
  const shown = screen.getByRole("status", { name: "Saved message" });
  expect(shown.textContent).toContain("Sending the message…");
  expect(shown.textContent).toContain("Original message");
  expect(screen.queryByRole("button", { name: "Retry message" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Discard message" })).toBeNull();
});

it("shows a failed message with its cause, and Retry resends the original while preserving newer text", async () => {
  const f = await fixture();
  await screen.findByText("The message wasn't sent");
  screen.getByText(/The runtime connection was lost/);
  const retry = screen.getByRole("button", { name: "Retry message" });
  retry.focus();
  expect(retry).toHaveFocus();
  fireEvent.click(retry);
  await waitFor(() =>
    expect(screen.queryByRole("status", { name: "Saved message" })).toBeNull(),
  );
  expect(f.control).toHaveBeenCalledTimes(1);
  expect(f.control).toHaveBeenCalledWith(f.request);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
  expect(f.app.drafts.getState().drafts.newer).toBe("Keep this newer draft");
});

it("discards a failed message without sending it", async () => {
  const f = await fixture();
  fireEvent.click(await screen.findByRole("button", { name: "Discard message" }));
  await waitFor(() =>
    expect(screen.queryByRole("status", { name: "Saved message" })).toBeNull(),
  );
  expect(f.control).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts).toEqual([]);
  expect(f.app.drafts.getState().drafts.newer).toBe("Keep this newer draft");
});

it("hands a discarded message's text back to an empty composer", async () => {
  const f = await fixture();
  const key = f.app.view.getState().draftKey;
  fireEvent.click(await screen.findByRole("button", { name: "Discard message" }));
  await waitFor(() =>
    expect(f.app.drafts.getState().drafts[key]).toBe("Original message"),
  );
});

it("leaves text typed since the send in the composer when the message is discarded", async () => {
  const f = await fixture();
  const key = f.app.view.getState().draftKey;
  act(() => f.app.drafts.setState({ drafts: { [key]: "Something new" } }));
  fireEvent.click(await screen.findByRole("button", { name: "Discard message" }));
  await waitFor(() =>
    expect(screen.queryByRole("status", { name: "Saved message" })).toBeNull(),
  );
  expect(f.app.drafts.getState().drafts[key]).toBe("Something new");
});

it("hands a discarded message and its attachments back to its own session while another is shown", async () => {
  const notes: RequestAttachment = {
    name: "notes.txt",
    mediaType: "text/plain",
    data: btoa("notes"),
  };
  const f = await fixture(false, "original-session", [notes]);
  const own = sessionDraftKey(f.request.nodeDid, "original-session");
  const discarding = deferred();
  const discard = f.app.actions.discardChatMessage;
  vi.spyOn(f.app.actions, "discardChatMessage").mockImplementation(
    async (requestId) => {
      await discarding.promise;
      return discard(requestId);
    },
  );
  fireEvent.click(await screen.findByRole("button", { name: "Discard message" }));
  act(() => f.app.stores.selection.setState({ sessionId: "another-session" }));
  const shown = f.app.view.getState().draftKey;
  expect(shown).not.toBe(own);
  await act(async () => discarding.resolve());
  await waitFor(() =>
    expect(f.app.drafts.getState().drafts[own]).toBe("Original message"),
  );
  expect(f.app.drafts.getState().attachments[own]).toEqual([
    expect.objectContaining({ name: "notes.txt", content: notes }),
  ]);
  expect(f.app.drafts.getState().drafts[shown]).toBeUndefined();
  expect(f.app.drafts.getState().attachments[shown]).toBeUndefined();
});

it("keeps a failed retry saved with its new cause", async () => {
  const f = await fixture();
  f.control.mockRejectedValueOnce(new Error("The selected agent was removed"));
  fireEvent.click(await screen.findByRole("button", { name: "Retry message" }));
  await screen.findAllByText(/The selected agent was removed/);
  expect(loadChatDrafts().chatDrafts[0].failure?.message).toBe(
    "The selected agent was removed",
  );
  expect(screen.getByRole("button", { name: "Retry message" })).toBeEnabled();
});

it("retries without navigating once its view closes", async () => {
  const f = await fixture();
  let finish!: () => void;
  f.control.mockImplementationOnce(
    (request) =>
      new Promise((resolve) => {
        finish = () => resolve(acceptedChat(request));
      }),
  );
  fireEvent.click(await screen.findByRole("button", { name: "Retry message" }));
  await waitFor(() => expect(f.control).toHaveBeenCalledTimes(1));
  f.view.unmount();
  finish();
  await waitFor(() => expect(loadChatDrafts().chatDrafts).toEqual([]));
  expect(f.app.stores.selection.getState().sessionId).toBeNull();
  expect(navigate).not.toHaveBeenCalled();
});

it("stops showing a failed message as unsent once its session shows it", async () => {
  const f = await fixture();
  await screen.findByText("The message wasn't sent");
  act(() =>
    writeSession(f.app.stores.session, {
      sessionId: f.request.sessionId!,
      nodeDid: f.request.nodeDid,
      latestRequestId: f.request.requestId!,
      pendingTurn: null,
      queuedTurns: [],
      foldedInputs: [],
      timelineItems: [],
    } as unknown as DesktopSessionSnapshot),
  );
  await waitFor(() =>
    expect(screen.queryByRole("status", { name: "Saved message" })).toBeNull(),
  );
  expect(f.control).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts).toEqual([]);
});
