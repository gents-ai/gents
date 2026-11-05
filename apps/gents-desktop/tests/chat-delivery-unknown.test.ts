import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { createMailboxActions } from "../src/hooks/mailboxActions";
import {
  loadUnresolvedSubmissions,
  contentKey,
  rememberUnresolvedSubmission,
} from "../src/hooks/unresolvedSubmissions";
import { shellStores } from "./shell-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());
const unknownOutcome = {
  nodeDid: "agent",
  agentDid: "setup",
  sessionId: "session-1",
  requestId: "request-unknown",
  outcome: "deliveryUnknown",
};

it("preserves exact resolution of an already saved uncertain mailbox answer", async () => {
  const sendChatMessage = vi
    .fn()
    .mockResolvedValueOnce(unknownOutcome)
    .mockResolvedValueOnce({ ...unknownOutcome, outcome: null });
  rememberUnresolvedSubmission({
    nodeDid: "agent",
    sessionId: "session-1",
    selectedSessionId: "session-1",
    requestId: "request-unknown",
    contentKey: contentKey("mailbox-answer:question-1"),
  });
  const stores = shellStores({
    deployments: [],
    selection: { nodeDid: "agent", agentDid: "setup", sessionId: "session-1" },
  });
  const reportFailure = vi.fn();
  const mailbox = createMailboxActions({
    api: { sendChatMessage } as unknown as DesktopApiAdapter,
    stores,
    refreshSnapshot: async () => {},
    reportFailure,
  } as never);
  const item = {
    itemId: "question-1",
    nodeDid: "agent",
    targetNodeDid: "agent",
    targetAgentDid: "setup",
    sessionId: "session-1",
  } as never;

  await mailbox.answerMailboxQuestion(item, { choice: "yes" } as never);
  expect(loadUnresolvedSubmissions()).toHaveLength(1);
  await mailbox.answerMailboxQuestion(item, { choice: "no" } as never);

  expect(sendChatMessage).toHaveBeenLastCalledWith(
    expect.objectContaining({ resolveRequestId: "request-unknown" }),
  );
  expect(loadUnresolvedSubmissions()).toEqual([]);
  expect(reportFailure).toHaveBeenLastCalledWith(
    "Your earlier answer was delivered; this retry did not send a different answer.",
  );
});

it("shows a receipt access denial while keeping delivery unresolved", async () => {
  const sendChatMessage = vi.fn().mockResolvedValue({
    ...unknownOutcome,
    deliveryError: { code: "permissionDenied", message: "Access denied", retryable: false, endpoint: null },
  });
  rememberUnresolvedSubmission({
    nodeDid: "agent", sessionId: "session-1", selectedSessionId: "session-1",
    requestId: "request-unknown", contentKey: contentKey("mailbox-answer:question-1"),
  });
  const stores = shellStores({
    deployments: [],
    selection: { nodeDid: "agent", agentDid: "setup", sessionId: "session-1" },
  });
  const reportFailure = vi.fn();
  const mailbox = createMailboxActions({
    api: { sendChatMessage } as unknown as DesktopApiAdapter,
    stores, refreshSnapshot: async () => {}, reportFailure,
  } as never);
  const item = {
    itemId: "question-1", nodeDid: "agent", targetNodeDid: "agent",
    targetAgentDid: "setup", sessionId: "session-1",
  } as never;
  await mailbox.answerMailboxQuestion(item, { choice: "yes" } as never);
  expect(sendChatMessage).toHaveBeenCalledTimes(1);
  expect(sendChatMessage).toHaveBeenCalledWith(expect.objectContaining({ resolveRequestId: "request-unknown" }));
  expect(loadUnresolvedSubmissions()[0].requestId).toBe("request-unknown");
  expect(reportFailure).toHaveBeenCalledWith(expect.stringContaining("Access denied"));
});

it("reports cleanup denial after an earlier answer was delivered", async () => {
  const sendChatMessage = vi.fn().mockResolvedValue({
    ...unknownOutcome,
    outcome: null,
    deliveryError: {
      code: "permissionDenied",
      message: "Access denied",
      retryable: false,
      endpoint: null,
    },
  });
  rememberUnresolvedSubmission({
    nodeDid: "agent",
    sessionId: "session-1",
    selectedSessionId: "session-1",
    requestId: "request-unknown",
    contentKey: contentKey("mailbox-answer:question-1"),
  });
  const stores = shellStores({
    deployments: [],
    selection: { nodeDid: "agent", agentDid: "setup", sessionId: "session-1" },
  });
  const reportFailure = vi.fn();
  const mailbox = createMailboxActions({
    api: { sendChatMessage } as unknown as DesktopApiAdapter,
    stores,
    refreshSnapshot: async () => {},
    reportFailure,
  } as never);
  const item = {
    itemId: "question-1",
    nodeDid: "agent",
    targetNodeDid: "agent",
    targetAgentDid: "setup",
    sessionId: "session-1",
  } as never;
  expect(await mailbox.answerMailboxQuestion(item, { choice: "yes" } as never)).toBe(
    true,
  );
  expect(sendChatMessage).toHaveBeenCalledTimes(1);
  expect(loadUnresolvedSubmissions()).toEqual([]);
  expect(reportFailure).toHaveBeenLastCalledWith(
    "Your earlier answer was delivered, but its saved-answer cleanup failed: Access denied",
  );
});
