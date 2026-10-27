import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  ChatSubmissionRecovery,
} from "@source-inc/gents-desktop-client";
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

function fixture() {
  const nodeDid = node().nodeDid;
  const request: ChatSendRequest = {
    clientDid: "client",
    nodeDid,
    requestId: "original-request",
    sessionId: "original-session",
    agentId: "default",
    content: "Original message",
    cwd: "/original",
  };
  const read = vi
    .fn<() => Promise<ChatSubmissionRecovery>>()
    .mockResolvedValue({ kind: "unconfirmed", nodeDid, requestId: "original-request" });
  const control = vi.fn(async (request: ChatSendRequest) => acceptedChat(request));
  const ack = vi.fn().mockRejectedValue(new Error("Lost acknowledgement"));
  const app = testApp({
    api: {
      recoverChatSubmission: read,
      sendChatMessage: control,
      acknowledgeChatSubmission: ack,
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
  saveChatDraft(app.stores.chat, retainChatDraft(request, null));
  const view = render(<ChatDeliveryFeedback />, { wrapper: withApp(app) });
  return { app, request, read, control, ack, view };
}

it("checks without dispatch and explicitly resumes the retained message while preserving newer text", async () => {
  const f = fixture();
  const check = screen.getByRole("button", { name: "Check message delivery" });
  check.focus();
  expect(check).toHaveFocus();
  fireEvent.click(check);
  await waitFor(() => expect(f.read).toHaveBeenCalledTimes(1));
  expect(f.control).not.toHaveBeenCalled();
  await screen.findByText(
    "Delivery is still unconfirmed. Check again or resume the saved message.",
  );
  fireEvent.click(screen.getByRole("button", { name: "Resume saved message" }));
  await screen.findByText("Message sent");
  expect(f.control).toHaveBeenCalledWith(f.request);
  expect(f.app.drafts.getState().drafts.newer).toBe("Keep this newer draft");
  expect(screen.queryByRole("button", { name: "Resume saved message" })).toBeNull();
});

it("shows a verified refusal and dismisses only that saved change", async () => {
  const f = fixture();
  f.read.mockResolvedValue({
    kind: "prepared",
    result: { ...acceptedChat(f.request), outcome: "notDelivered" },
  });
  f.ack.mockResolvedValue(undefined);
  fireEvent.click(screen.getByRole("button", { name: "Check message delivery" }));
  await screen.findByText("Message was not delivered");
  expect(screen.queryByRole("button", { name: "Resume saved message" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Dismiss saved message" }));
  await waitFor(() =>
    expect(screen.queryByRole("status", { name: "Saved message" })).toBeNull(),
  );
  expect(f.control).not.toHaveBeenCalled();
  expect(f.app.drafts.getState().drafts.newer).toBe("Keep this newer draft");
});

it("disables recovery while the original call is still in flight", () => {
  const f = fixture();
  f.app.stores.chat.setState({ chatRequestsInFlight: new Set(["original-request"]) });
  return waitFor(() => {
    expect(
      screen.getByRole("button", { name: "Check message delivery" }),
    ).toBeDisabled();
    expect(screen.getByRole("button", { name: "Resume saved message" })).toBeDisabled();
  });
});

it("retains a confirmed result after its recovery view closes without navigating", async () => {
  const f = fixture();
  let finish!: (value: ChatSubmissionRecovery) => void;
  f.read.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  f.ack.mockResolvedValue(undefined);
  fireEvent.click(screen.getByRole("button", { name: "Check message delivery" }));
  await waitFor(() => expect(f.read).toHaveBeenCalledTimes(1));
  f.view.unmount();
  finish({ kind: "prepared", result: acceptedChat(f.request) });
  await waitFor(() => expect(f.ack).toHaveBeenCalledTimes(1));
  expect(f.app.stores.selection.getState().sessionId).toBeNull();
  expect(navigate).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts[0].result).toEqual(acceptedChat(f.request));
  expect(f.app.drafts.getState().drafts.newer).toBe("Keep this newer draft");
});
