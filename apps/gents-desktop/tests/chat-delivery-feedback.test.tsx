import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ChatSendRequest } from "@source-inc/gents-desktop-client";
import { ChatDeliveryFeedback } from "../src/ui/screens/session/ChatDeliveryFeedback";
import { retainChatDraft, saveChatDraft } from "../src/hooks/chatDrafts";
import { acceptedChat } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { loadChatDrafts } from "../src/hooks/chatDrafts";
import { navigate } from "../src/ui/lib/router";
import { node, testApp, withApp } from "./app-fixture";

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
  if (sending)
    app.stores.chat.setState({ chatRequestsInFlight: new Set(["original-request"]) });
  saveChatDraft(app.stores.chat, {
    ...retainChatDraft(request, null),
    failure: { message: "The runtime connection was lost", error: null },
  });
  const view = render(<ChatDeliveryFeedback />, { wrapper: withApp(app) });
  return { app, request, control, view };
}

it("shows nothing while a message is being sent", () => {
  fixture(true);
  expect(screen.queryByRole("status", { name: "Saved message" })).toBeNull();
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
