import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { createMailboxActions } from "../src/hooks/mailboxActions";
import { loadUnresolvedSubmissions } from "../src/hooks/unresolvedSubmissions";
import { shellStores } from "./shell-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());
const unknownOutcome = {
  nodeDid: "agent",
  agentId: "setup",
  sessionId: "session-1",
  requestId: "request-unknown",
  outcome: "deliveryUnknown",
};

it("reconciles a mailbox answer whose delivery is unknown", async () => {
  const sendChatMessage = vi
    .fn()
    .mockResolvedValueOnce(unknownOutcome)
    .mockResolvedValueOnce({ ...unknownOutcome, outcome: null });
  const stores = shellStores({
    selection: { nodeDid: "agent", agentId: "setup", sessionId: "session-1" },
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
    targetNodeDid: "agent",
    targetAgentId: "setup",
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
