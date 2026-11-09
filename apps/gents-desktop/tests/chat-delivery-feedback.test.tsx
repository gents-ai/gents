import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ChatSendRequest } from "@source-inc/gents-desktop-client";
import { ChatDeliveryFeedback } from "../src/ui/screens/session/ChatDeliveryFeedback";
import { retainChatDraft, saveChatDraft } from "../src/hooks/chatDrafts";
import { acceptedChat } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { loadChatDrafts } from "../src/hooks/chatDrafts";
import { navigate } from "../src/ui/lib/router";
import { node, testApp, withApp } from "./app-fixture";
import { writeSession } from "../src/hooks/sessionStore";
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

function fixture(sending = false) {
  const nodeDid = node().nodeDid;
  const request: ChatSendRequest = {
    clientDid: "client",
    nodeDid,
    requestId: "original-request",
    sessionId: "original-session",
    agentDid: "default",
    content: "Original message",
    cwd: "/original",
  };
  const control = vi.fn(async (request: ChatSendRequest) => acceptedChat(request));
  const app = testApp({
    api: {
      sendChatMessage: control,
      fetchSessionSnapshot: vi.fn().mockResolvedValue(null),
    },
    deployments: [node()],
    selection: { nodeDid, sessionId: null },
  });
  app.stores.client.setState((state) => ({
    snapshot: {
      ...state.snapshot!,
      client: { ...state.snapshot!.client!, clientDid: "client" },
    },
  }));
  app.drafts.setState({ drafts: { newer: "Keep this newer draft" } });
  saveChatDraft(
    app.stores.chat,
    sending
      ? { ...retainChatDraft(request, null), pendingUntil: Date.now() + 60_000 }
      : {
          ...retainChatDraft(request, null),
          failure: { message: "The runtime connection was lost", error: null },
        },
  );
  const view = render(<ChatDeliveryFeedback />, { wrapper: withApp(app) });
  return { app, request, control, view };
}

it("shows a message as sending from the moment it is sent, with no actions", () => {
  fixture(true);
  const shown = screen.getByRole("status", { name: "Saved message" });
  expect(shown.textContent).toContain("Sending the message…");
  expect(shown.textContent).toContain("Original message");
  expect(screen.queryByRole("button", { name: "Retry message" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Discard message" })).toBeNull();
});

it("shows a failed message with its cause, and Retry resends the original while preserving newer text", async () => {
  const f = fixture();
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
  const f = fixture();
  fireEvent.click(await screen.findByRole("button", { name: "Discard message" }));
  await waitFor(() =>
    expect(screen.queryByRole("status", { name: "Saved message" })).toBeNull(),
  );
  expect(f.control).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts).toEqual([]);
  expect(f.app.drafts.getState().drafts.newer).toBe("Keep this newer draft");
});

it("hands a discarded message's text back to an empty composer", async () => {
  const f = fixture();
  const key = f.app.view.getState().draftKey;
  fireEvent.click(await screen.findByRole("button", { name: "Discard message" }));
  await waitFor(() =>
    expect(f.app.drafts.getState().drafts[key]).toBe("Original message"),
  );
});

it("leaves text typed since the send in the composer when the message is discarded", async () => {
  const f = fixture();
  const key = f.app.view.getState().draftKey;
  act(() => f.app.drafts.setState({ drafts: { [key]: "Something new" } }));
  fireEvent.click(await screen.findByRole("button", { name: "Discard message" }));
  await waitFor(() =>
    expect(screen.queryByRole("status", { name: "Saved message" })).toBeNull(),
  );
  expect(f.app.drafts.getState().drafts[key]).toBe("Something new");
});

it("keeps a failed retry saved with its new cause", async () => {
  const f = fixture();
  f.control.mockRejectedValueOnce(new Error("The selected agent was removed"));
  fireEvent.click(await screen.findByRole("button", { name: "Retry message" }));
  await screen.findAllByText(/The selected agent was removed/);
  expect(loadChatDrafts().chatDrafts[0].failure?.message).toBe(
    "The selected agent was removed",
  );
  expect(screen.getByRole("button", { name: "Retry message" })).toBeEnabled();
});

it("retries without navigating once its view closes", async () => {
  const f = fixture();
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
  const f = fixture();
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
