import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  BridgeErrorPayload,
  ChatSendRequest,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { ChatDeliveryFeedback } from "../src/ui/screens/session/ChatDeliveryFeedback";
import {
  loadChatDrafts,
  retainChatDraft,
  saveChatDraft,
} from "../src/hooks/chatDrafts";
import { node, renderIn, testApp } from "./app-fixture";
import { acceptedChat } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());
const failure: BridgeErrorPayload = {
  code: "storageFull",
  message: "Storage is full",
  retryable: false,
  endpoint: null,
};

function fixture(error = failure) {
  const nodeDid = node().nodeDid;
  const request: ChatSendRequest = {
    clientDid: "test-client",
    nodeDid,
    requestId: "original-request",
    sessionId: "original-session",
    agentId: "default",
    content: "Keep my original message",
    cwd: "/workspace",
  };
  const status: ManagedServerStatus = {
    state: "running",
    nodeDid,
    nodeName: "Workshop",
    graphql: "http://127.0.0.1:9191/graphql",
    autoStart: true,
    effectiveToolCeiling: "readonly",
    effectiveToolRoot: "/workspace",
    suggestedToolRoot: "/workspace",
    pairingReady: true,
    approvalRequired: false,
    runtimeBooting: false,
    error: null,
  };
  const result = {
    ...acceptedChat(request),
    outcome: "deliveryUnknown" as const,
    deliveryError: error,
  };
  const managedServerStatus = vi.fn(async () => status);
  const restartManagedServer = vi.fn(async () => status);
  const send = vi.fn(async (request: ChatSendRequest) => acceptedChat(request));
  const recover = vi.fn(async () => ({ kind: "prepared" as const, result }));
  const app = testApp({
    deployments: [node()],
    selection: { nodeDid, sessionId: null },
    api: {
      managedServerStatus,
      restartManagedServer,
      sendChatMessage: send,
      recoverChatSubmission: recover,
      acknowledgeChatSubmission: vi.fn().mockResolvedValue(undefined),
    },
  });
  app.api.fetchDesktopSnapshot = vi.fn(
    async () => app.stores.client.getState().snapshot!,
  );
  app.drafts.setState({ drafts: { newer: "Keep this newer draft too" } });
  const draft = retainChatDraft(request, null);
  saveChatDraft(app.stores.chat, { ...draft, result });
  const render = () =>
    renderIn(
      app,
      <TooltipProvider>
        <ChatDeliveryFeedback />
      </TooltipProvider>,
    );
  const view = render();
  return {
    app,
    request,
    result,
    status,
    managedServerStatus,
    restartManagedServer,
    send,
    recover,
    view,
    render,
  };
}

async function review() {
  fireEvent.click(screen.getByRole("button", { name: "Review runtime restart…" }));
  await screen.findByRole("alertdialog");
}

it("requires review, preserves access and delivery identity across restart/reload, and resumes only explicitly", async () => {
  const f = fixture();
  expect(screen.getByLabelText("Storage recovery")).toHaveTextContent(
    "Free space on its host",
  );
  expect(f.restartManagedServer).not.toHaveBeenCalled();
  expect(f.managedServerStatus).not.toHaveBeenCalled();
  await review();
  expect(screen.getByRole("alertdialog")).toHaveTextContent("/workspace");
  expect(f.restartManagedServer).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Restart runtime" }));
  await screen.findByText(/The runtime restarted/);
  expect(f.restartManagedServer).toHaveBeenCalledExactlyOnceWith("Workshop", {
    toolCeiling: "readonly",
    toolRoot: "/workspace",
  });
  expect(f.send).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts[0]).toMatchObject({
    requestId: f.request.requestId,
    result: f.result,
    requestJson: JSON.stringify(f.request),
  });
  expect(f.app.drafts.getState().drafts.newer).toBe("Keep this newer draft too");
  f.view.unmount();
  f.render();
  fireEvent.click(screen.getByRole("button", { name: "Check message delivery" }));
  await waitFor(() => expect(f.recover).toHaveBeenCalledOnce());
  expect(f.send).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Resume saved message" }));
  await waitFor(() => expect(f.send).toHaveBeenCalledExactlyOnceWith(f.request));
});

it("cancel leaves the runtime and saved message untouched", async () => {
  const f = fixture();
  await review();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
  expect(f.restartManagedServer).not.toHaveBeenCalled();
  expect(f.send).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts[0].result).toEqual(f.result);
});

it.each(["external", "starting"] as const)(
  "refuses to restart a service whose fresh state is %s",
  async (state) => {
    const f = fixture();
    f.managedServerStatus.mockResolvedValue({ ...f.status, state });
    fireEvent.click(screen.getByRole("button", { name: "Review runtime restart…" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Restart this node on its host",
    );
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(f.restartManagedServer).not.toHaveBeenCalled();
  },
);

it("does not restart a different node if status changes after confirmation", async () => {
  const f = fixture();
  await review();
  f.managedServerStatus.mockResolvedValue({ ...f.status, nodeDid: "did:key:other" });
  fireEvent.click(screen.getByRole("button", { name: "Restart runtime" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Restart this node on its host",
  );
  expect(f.restartManagedServer).not.toHaveBeenCalled();
});

it("requires another review when the managed authority changes", async () => {
  const f = fixture();
  await review();
  f.managedServerStatus.mockResolvedValue({
    ...f.status,
    effectiveToolCeiling: "readwrite",
  });
  fireEvent.click(screen.getByRole("button", { name: "Restart runtime" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("access changed");
  expect(f.restartManagedServer).not.toHaveBeenCalled();
});

it("discards a confirmation when the client changes", async () => {
  const f = fixture();
  await review();
  act(() =>
    f.app.stores.client.setState((state) => ({
      snapshot: {
        ...state.snapshot!,
        client: { ...state.snapshot!.client!, clientDid: "replacement-client" },
      },
    })),
  );
  await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
  expect(f.restartManagedServer).not.toHaveBeenCalled();
});

it("keeps the failure visible when restart fails without replaying the message", async () => {
  const f = fixture();
  f.restartManagedServer.mockRejectedValue(new Error("Runtime shutdown refused"));
  await review();
  fireEvent.click(screen.getByRole("button", { name: "Restart runtime" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Runtime shutdown refused",
  );
  expect(f.send).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts[0].result).toEqual(f.result);
});

it("does not infer disk-full recovery from generic failure text", () => {
  const f = fixture({ ...failure, code: "unknown" });
  expect(screen.queryByLabelText("Storage recovery")).toBeNull();
  expect(f.managedServerStatus).not.toHaveBeenCalled();
});

it("does not report recovery when the service returns a failed restart", async () => {
  const f = fixture();
  f.restartManagedServer.mockResolvedValue({
    ...f.status,
    state: "failed",
    error: "Store could not be reopened",
  });
  await review();
  fireEvent.click(screen.getByRole("button", { name: "Restart runtime" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("restart did not confirm");
  expect(screen.queryByText(/The runtime restarted/)).toBeNull();
  expect(f.send).not.toHaveBeenCalled();
});

it.each(["notDelivered", null] as const)(
  "removes disk-full recovery once delivery is authoritatively %s",
  (outcome) => {
    const f = fixture();
    act(() =>
      saveChatDraft(f.app.stores.chat, {
        ...retainChatDraft(f.request, null),
        result: { ...f.result, outcome },
      }),
    );
    expect(screen.queryByLabelText("Storage recovery")).toBeNull();
    expect(f.restartManagedServer).not.toHaveBeenCalled();
  },
);
