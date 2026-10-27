import {
  contentKey,
  rememberUnresolvedSubmission,
} from "../src/hooks/unresolvedSubmissions";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  ChatSendResult,
  ChatSubmissionRecovery,
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { createChatActions } from "../src/hooks/chatActions";
import { createChatStore } from "../src/hooks/chatStore";
import { chatDraftKey, loadChatDrafts } from "../src/hooks/chatDrafts";
import { selection } from "../src/hooks/selectionStore";
import { admittingProjection, shellStores } from "./shell-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { acceptedChat } from "./chat-delivery-fixture";
import { deferred } from "./deferred";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());

function fixture(sessionId: string | null = null) {
  const stores = shellStores({
    deployments: [{ nodeDid: "node", sessions: [], mailboxItems: [] }],
    selection: { nodeDid: "node", sessionId },
  });
  stores.client.setState({
    snapshot: { client: { clientDid: "client" } } as DesktopClientSnapshot,
  });
  const send = vi
    .fn<(request: ChatSendRequest) => Promise<ChatSendResult>>()
    .mockRejectedValue(new Error("Outer reply was lost"));
  const read = vi.fn<() => Promise<ChatSubmissionRecovery>>();
  const ack = vi
    .fn<() => Promise<void>>()
    .mockRejectedValue(new Error("Acknowledgement reply was lost"));
  const report = vi.fn();
  const refresh = vi.fn();
  const projection = admittingProjection("agent");
  const actions = () =>
    createChatActions({
      api: {
        sendChatMessage: send,
        recoverChatSubmission: read,
        acknowledgeChatSubmission: ack,
      } as unknown as DesktopApiAdapter,
      stores,
      project: () => projection,
      refreshSession: refresh,
      refreshSnapshot: vi.fn(),
      reportFailure: report,
    });
  return { stores, send, read, ack, report, refresh, projection, actions };
}
function prepared(
  request: ChatSendRequest,
  outcome?: "deliveryUnknown" | "notDelivered",
): ChatSubmissionRecovery {
  return { kind: "prepared", result: { ...acceptedChat(request), outcome } };
}

it("saves the full authored input and caller identity before the first effect", async () => {
  const f = fixture("session");
  f.projection.shellProjection.nonEmptyContentSendStatus = {
    kind: "queue",
    turnState: "running",
    hint: "Queue",
  };
  selection.openMailboxRoute(f.stores.selection, {
    itemId: "source",
    nodeDid: "node",
    agentId: "agent",
    sessionId: "session",
  });
  const actions = f.actions();
  actions.setChatFolder("/original");
  f.send.mockImplementationOnce(async (request) => {
    expect(JSON.parse(loadChatDrafts().chatDrafts[0].requestJson)).toEqual(request);
    expect(request).toMatchObject({
      clientDid: "client",
      requestId: expect.any(String),
      sessionId: "session",
      agentId: "agent",
      content: "Original message",
      cwd: "/original",
      delivery: "steer",
      causedBySourceDocId: "source",
      attachments: [{ name: "note.txt", mediaType: "text/plain", data: "bm90ZQ==" }],
    });
    throw new Error("Lost reply");
  });
  await actions.sendMessage(
    "Original message",
    undefined,
    [{ name: "note.txt", mediaType: "text/plain", data: "bm90ZQ==" }],
    "steer",
  );
  expect(f.send).toHaveBeenCalledTimes(1);
  expect(loadChatDrafts().chatDrafts[0].queued).toBe(true);
});

it("retains a lost opening through reload and explicitly resumes only its original input", async () => {
  const f = fixture();
  f.actions().setChatFolder("/original");
  await f.actions().sendMessage("Original message");
  const request = f.send.mock.calls[0][0];
  f.stores.chat = createChatStore();
  const reloaded = f.actions();
  reloaded.setChatFolder("/newer");
  f.read.mockResolvedValue({
    kind: "unconfirmed",
    nodeDid: "node",
    requestId: request.requestId!,
  });
  expect(await reloaded.recoverChatMessage(request.requestId!)).toBeNull();
  expect(f.send).toHaveBeenCalledTimes(1);
  f.send.mockImplementationOnce(async (original) => acceptedChat(original));
  await reloaded.recoverChatMessage(request.requestId!, true);
  expect(f.send.mock.calls[1][0]).toEqual(request);
  expect(f.stores.selection.getState().sessionId).toBe(request.sessionId);
  expect(f.stores.chat.getState().folders).toMatchObject({
    "": "/newer",
    [request.sessionId!]: "/original",
  });
});

it("queries a prepared result without publishing and resumes its byte-identical authored command", async () => {
  const f = fixture("session");
  await f.actions().sendMessage("Original message");
  const request = f.send.mock.calls[0][0];
  f.read.mockResolvedValue(prepared(request, "deliveryUnknown"));
  await f.actions().recoverChatMessage(request.requestId!);
  expect(f.send).toHaveBeenCalledTimes(1);
  f.send.mockImplementationOnce(async (original) => acceptedChat(original));
  await f.actions().recoverChatMessage(request.requestId!, true);
  expect(f.send.mock.calls[1][0]).toEqual(request);
});

it("never signs anew when a previously observed signed preparation becomes unavailable", async () => {
  const f = fixture();
  await f.actions().sendMessage("Original message");
  const request = f.send.mock.calls[0][0];
  f.read.mockResolvedValueOnce(prepared(request, "deliveryUnknown"));
  await f.actions().recoverChatMessage(request.requestId!);
  f.stores.chat = createChatStore();
  f.read.mockResolvedValue({
    kind: "unconfirmed",
    nodeDid: "node",
    requestId: request.requestId!,
  });
  await expect(
    f.actions().recoverChatMessage(request.requestId!, true),
  ).rejects.toThrow("original signed preparation is unavailable");
  expect(f.send).toHaveBeenCalledTimes(1);
  expect(f.ack).not.toHaveBeenCalled();
});

it("persists confirmation before acknowledgement and reconciles a lost acknowledgement without resubmission", async () => {
  const f = fixture();
  f.send.mockImplementationOnce(async (request) => acceptedChat(request));
  await f.actions().sendMessage("Original message");
  const request = f.send.mock.calls[0][0];
  await vi.waitFor(() => expect(f.ack).toHaveBeenCalledTimes(1));
  expect(loadChatDrafts().chatDrafts[0].result).toEqual(acceptedChat(request));
  f.stores.chat = createChatStore();
  f.read.mockResolvedValue(prepared(request));
  f.ack.mockImplementationOnce(async () => {
    expect(loadChatDrafts().chatDrafts[0].result).toEqual(acceptedChat(request));
  });
  await f.actions().recoverChatMessage(request.requestId!);
  await vi.waitFor(() => expect(loadChatDrafts().chatDrafts).toHaveLength(0));
  expect(f.send).toHaveBeenCalledTimes(1);
});

it.each(["node", "request", "session", "Agent", "receipt", "signature"])(
  "rejects a mismatched %s result without acknowledgement",
  async (field) => {
    const f = fixture();
    await f.actions().sendMessage("Original message");
    const request = f.send.mock.calls[0][0];
    const reply = prepared(request);
    if (reply.kind !== "prepared") throw new Error("fixture");
    if (field === "node") reply.result.nodeDid = "other";
    if (field === "request") reply.result.requestId = "other";
    if (field === "session") reply.result.sessionId = "other";
    if (field === "Agent") reply.result.agentId = "other";
    if (field === "receipt") reply.result.submissionReceipt = null;
    if (field === "signature") {
      f.read.mockResolvedValueOnce(prepared(request, "deliveryUnknown"));
      await f.actions().recoverChatMessage(request.requestId!);
      reply.result.submissionReceipt!.signedPayloadHash = "changed";
    }
    f.read.mockResolvedValue(reply);
    await expect(f.actions().recoverChatMessage(request.requestId!)).rejects.toThrow();
    expect(f.ack).not.toHaveBeenCalled();
    expect(f.stores.selection.getState().sessionId).toBeNull();
  },
);

it("requires confirmed non-delivery before dismissal and never treats an absent result as proof", async () => {
  const f = fixture();
  await f.actions().sendMessage("Original message");
  const request = f.send.mock.calls[0][0];
  f.read.mockResolvedValue({
    kind: "unconfirmed",
    nodeDid: "node",
    requestId: request.requestId!,
  });
  await expect(f.actions().dismissChatMessage(request.requestId!)).rejects.toThrow(
    "non-delivery is not confirmed",
  );
  f.read.mockResolvedValue(prepared(request, "notDelivered"));
  await f.actions().recoverChatMessage(request.requestId!, true);
  expect(loadChatDrafts().chatDrafts[0].result?.outcome).toBe("notDelivered");
  await f.actions().dismissChatMessage(request.requestId!);
  expect(loadChatDrafts().chatDrafts).toHaveLength(0);
  expect(f.send).toHaveBeenCalledTimes(1);
});

it("does not replace an unresolved operation with new text", async () => {
  const f = fixture();
  await f.actions().sendMessage("Original message");
  await f.actions().sendMessage("A newer draft");
  expect(f.send).toHaveBeenCalledTimes(1);
  expect(JSON.parse(loadChatDrafts().chatDrafts[0].requestJson).content).toBe(
    "Original message",
  );
});

it("treats repeated text after confirmation as two distinct authored requests", async () => {
  const f = fixture("session");
  f.send.mockImplementation(async (request) => acceptedChat(request));
  await f.actions().sendMessage("Yes");
  await f.actions().sendMessage("Yes");
  expect(f.send).toHaveBeenCalledTimes(2);
  expect(f.send.mock.calls[0][0].requestId).not.toBe(f.send.mock.calls[1][0].requestId);
});

it("refuses dispatch if the caller form cannot be saved", async () => {
  const f = fixture();
  vi.spyOn(localStorage, "setItem").mockImplementation(() => {
    throw new Error("Full");
  });
  await f.actions().sendMessage("Original message");
  expect(f.send).not.toHaveBeenCalled();
});

it("does not acknowledge if the confirmed caller result cannot be saved", async () => {
  const f = fixture();
  f.send.mockImplementationOnce(async (request) => {
    vi.spyOn(localStorage, "setItem").mockImplementation(() => {
      throw new Error("Full");
    });
    return acceptedChat(request);
  });
  expect(await f.actions().sendMessage("Original message")).toBeNull();
  expect(f.ack).not.toHaveBeenCalled();
});

it("keeps a late confirmed reply without selecting or refreshing another chat", async () => {
  const f = fixture();
  const reply = deferred<ChatSendResult>();
  f.send.mockReturnValueOnce(reply.promise);
  f.ack.mockResolvedValue(undefined);
  const pending = f.actions().sendMessage("Original message");
  const request = f.send.mock.calls[0][0];
  selection.selectSession(f.stores.selection, "other-chat");
  reply.resolve(acceptedChat(request));
  await pending;
  await vi.waitFor(() => expect(f.ack).toHaveBeenCalledTimes(1));
  expect(f.stores.selection.getState().sessionId).toBe("other-chat");
  expect(f.refresh).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts[0].result).toEqual(acceptedChat(request));
  f.read.mockResolvedValue(prepared(request));
  await f.actions().recoverChatMessage(request.requestId!);
  expect(f.refresh).not.toHaveBeenCalled();
  expect(f.stores.selection.getState().sessionId).toBe("other-chat");
});

it("does not recover an old client's message after switching identity", async () => {
  const f = fixture();
  await f.actions().sendMessage("Original message");
  const request = f.send.mock.calls[0][0];
  f.stores.client.setState({
    snapshot: { client: { clientDid: "other" } } as DesktopClientSnapshot,
  });
  await expect(
    f.actions().recoverChatMessage(request.requestId!, true),
  ).rejects.toThrow("unavailable for this client");
  expect(f.read).not.toHaveBeenCalled();
  expect(f.send).toHaveBeenCalledTimes(1);
});

it("can recover confirmation from an unreadable form without submitting it as new input", async () => {
  const f = fixture();
  await f.actions().sendMessage("Original message");
  const request = f.send.mock.calls[0][0];
  const draft = loadChatDrafts().chatDrafts[0];
  localStorage.setItem(
    chatDraftKey(draft),
    JSON.stringify({ ...draft, requestJson: "unreadable" }),
  );
  f.read.mockResolvedValue(prepared(request));
  await f.actions().recoverChatMessage(request.requestId!);
  expect(f.stores.selection.getState().sessionId).toBe(request.sessionId);
  expect(f.send).toHaveBeenCalledTimes(1);
  expect(f.stores.chat.getState().optimisticPendingTurn).toBeNull();
});

it("resolves an already retained request without allocating a replacement identity", async () => {
  const f = fixture("session");
  rememberUnresolvedSubmission({
    nodeDid: "node",
    sessionId: "session",
    selectedSessionId: "session",
    requestId: "earlier",
    contentKey: contentKey("Original message"),
  });
  f.send.mockResolvedValueOnce({
    nodeDid: "node",
    sessionId: "session",
    requestId: "earlier",
    agentId: "agent",
  });
  await f.actions().sendMessage("Original message");
  expect(f.send).toHaveBeenCalledWith(
    expect.objectContaining({ resolveRequestId: "earlier" }),
  );
  expect(f.send.mock.calls[0][0].requestId).toBeUndefined();
  expect(loadChatDrafts().chatDrafts).toHaveLength(0);
  f.send.mockImplementationOnce(async (request) => acceptedChat(request));
  await f.actions().sendMessage("Original message");
  expect(f.send.mock.calls[1][0].requestId).toEqual(expect.any(String));
});
