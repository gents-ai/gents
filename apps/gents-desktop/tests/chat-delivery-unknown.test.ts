import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  DeploymentView,
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";

import {
  createChatActions,
  DELIVERY_UNKNOWN_HINT,
  EARLIER_SEND_NOT_DELIVERED_HINT,
} from "../src/hooks/chatActions";
import { applyFleetSnapshot } from "../src/hooks/fleetStore";
import { createMailboxActions } from "../src/hooks/mailboxActions";
import { writeSession } from "../src/hooks/sessionStore";
import { loadUnresolvedSubmissions } from "../src/hooks/unresolvedSubmissions";
import { admittingProjection, shellStores } from "./shell-fixture";

const deployment = {
  nodeDid: "agent",
  node: { nodeDid: "agent", defaultAgentId: "setup" },
  agents: [{ agentId: "setup", enabled: true, isDefault: true }],
  sessions: [{ sessionId: "session-1", agentId: "setup" }],
  mailboxItems: [],
} as unknown as DeploymentView;

function harness(sendChatMessage: ReturnType<typeof vi.fn>) {
  const stores = shellStores({
    selection: { nodeDid: "agent", agentId: "setup", sessionId: "session-1" },
  });
  applyFleetSnapshot(stores.fleet, {
    bootstrap: {},
    client: { deployments: [deployment] },
  } as unknown as DesktopClientSnapshot);
  const reportFailure = vi.fn();
  const actions = createChatActions({
    api: { sendChatMessage } as unknown as DesktopApiAdapter,
    stores,
    project: () => admittingProjection("setup"),
    refreshSession: async () => null,
    refreshSnapshot: async () => {},
    reportFailure,
  });
  return { actions, reportFailure, stores };
}

const unknownOutcome = {
  nodeDid: "agent",
  agentId: "setup",
  sessionId: "session-1",
  requestId: "request-unknown",
  outcome: "deliveryUnknown",
};

describe("chat send with an unknown delivery outcome", () => {
  beforeEach(() => {
    const storage = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
      removeItem: (key: string) => storage.delete(key),
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("reconciles the original request instead of sending a new one", async () => {
    const sendChatMessage = vi
      .fn()
      .mockResolvedValueOnce(unknownOutcome)
      .mockResolvedValueOnce({ ...unknownOutcome, outcome: null });
    const { actions, reportFailure } = harness(sendChatMessage);

    await actions.sendMessage("hello owner");
    expect(reportFailure).toHaveBeenCalledWith(DELIVERY_UNKNOWN_HINT);
    expect(loadUnresolvedSubmissions()).toEqual([
      expect.objectContaining({
        requestId: "request-unknown",
        sessionId: "session-1",
      }),
    ]);

    await actions.sendMessage("hello owner");
    expect(sendChatMessage).toHaveBeenLastCalledWith(
      expect.objectContaining({
        sessionId: "session-1",
        content: "hello owner",
        resolveRequestId: "request-unknown",
      }),
    );
    expect(loadUnresolvedSubmissions()).toEqual([]);
  });

  it("keeps the identity unresolved after a reload when the owner shows no request", async () => {
    const first = harness(vi.fn().mockResolvedValueOnce(unknownOutcome));
    await first.actions.sendMessage("hello owner");

    // A new action owner models a reload: only the persisted identity survives,
    // and an absent owner receipt cannot prove the original never commits.
    const sendChatMessage = vi
      .fn()
      .mockResolvedValue({ ...unknownOutcome, agentId: null });
    const reloaded = harness(sendChatMessage);
    await reloaded.actions.sendMessage("hello owner");
    await reloaded.actions.sendMessage("hello owner");

    expect(sendChatMessage).toHaveBeenCalledTimes(2);
    for (const [call] of sendChatMessage.mock.calls)
      expect(call).toEqual(
        expect.objectContaining({ resolveRequestId: "request-unknown" }),
      );
    expect(reloaded.reportFailure).toHaveBeenCalledWith(DELIVERY_UNKNOWN_HINT);
    expect(reloaded.reportFailure).not.toHaveBeenCalledWith(
      EARLIER_SEND_NOT_DELIVERED_HINT,
    );
    expect(loadUnresolvedSubmissions()).toEqual([
      expect.objectContaining({ requestId: "request-unknown" }),
    ]);
  });

  it("sends different content normally", async () => {
    const sendChatMessage = vi
      .fn()
      .mockResolvedValueOnce(unknownOutcome)
      .mockResolvedValueOnce({
        ...unknownOutcome,
        requestId: "request-other",
        outcome: null,
      });
    const { actions } = harness(sendChatMessage);

    await actions.sendMessage("hello owner");
    await actions.sendMessage("a different message");

    expect(sendChatMessage).toHaveBeenLastCalledWith(
      expect.objectContaining({
        content: "a different message",
        resolveRequestId: null,
      }),
    );
    expect(loadUnresolvedSubmissions()).toHaveLength(1);
  });

  it("sends an identical message normally once the earlier one is visible", async () => {
    const sendChatMessage = vi
      .fn()
      .mockResolvedValueOnce(unknownOutcome)
      .mockResolvedValueOnce({
        ...unknownOutcome,
        requestId: "request-repeat",
        outcome: null,
      });
    const { actions, stores } = harness(sendChatMessage);

    await actions.sendMessage("yes");
    writeSession(stores.session, {
      nodeDid: "agent",
      sessionId: "session-1",
      latestRequestId: "request-unknown",
      foldedInputs: [],
      pendingQueue: [],
    } as never);
    await actions.sendMessage("yes");

    expect(sendChatMessage).toHaveBeenLastCalledWith(
      expect.objectContaining({ content: "yes", resolveRequestId: null }),
    );
  });

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
    await mailbox.answerMailboxQuestion(item, { choice: "yes" } as never);

    expect(sendChatMessage).toHaveBeenLastCalledWith(
      expect.objectContaining({ resolveRequestId: "request-unknown" }),
    );
    expect(loadUnresolvedSubmissions()).toEqual([]);
  });
});
