import { afterEach, expect, it, vi } from "vitest";
import type { ChatSendRequest, ChatSendResult } from "@source-inc/gents-desktop-client";
import { CHAT_SEND_DEADLINE_MS } from "../../src/hooks/chatDelivery";
import { createChatStore } from "../../src/hooks/chatStore";
import { loadChatDrafts } from "../../src/hooks/chatDrafts";
import { deliveryAffordances, sendStatus } from "../../src/hooks/chatDeliveryView";
import { goalDeliveryStorage } from "../goal-delivery-storage";
import { acceptedChat, chatDeliveryFixture } from "../chat-delivery-fixture";
import { deferred } from "../deferred";
import noPayload from "../../../../crates/gents/proofs/tla/generated/traces/MCClientSubmissionContentionNoPayload.json";
import freshId from "../../../../crates/gents/proofs/tla/generated/traces/MCClientSubmissionContentionRetryFreshId.json";

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

it.each([
  { trace: noPayload, payloadLost: true, retryFreshId: false },
  { trace: freshId, payloadLost: false, retryFreshId: true },
])(
  "replays $trace.config through the chat delivery owner",
  async ({ trace, payloadLost, retryFreshId }) => {
    expect(trace.spec).toBe("ClientSubmissionContention");
    expect(trace.constants).toMatchObject({
      PayloadLost: payloadLost,
      RetryFreshId: retryFreshId,
    });
    vi.useFakeTimers();
    vi.stubGlobal("localStorage", goalDeliveryStorage());
    const reply = deferred<ChatSendResult>();
    const send = vi.fn((_request: ChatSendRequest) => reply.promise);
    const { stores, delivery } = chatDeliveryFixture(send);
    const applied = async () => true;
    let submitted: Promise<unknown> | undefined;
    let committed = 0;
    const ids = new Set<string>();
    for (const step of trace.steps) {
      switch (step.action) {
        case "Compose":
          submitted = delivery
            .submit(
              {
                clientDid: "client",
                nodeDid: "node",
                agentDid: "agent",
                sessionId: "session",
                content: "Original message",
                attachments: [],
                delivery: null,
                cwd: null,
                causedBySourceDocId: null,
              },
              "session",
              false,
              applied,
            )
            .catch((error: unknown) => error);
          await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(1));
          break;
        case "SubmitCommit": {
          expect(step.actor).toBe(step.before.wire[0].id);
          const request = send.mock.calls[committed++][0];
          ids.add(request.requestId!);
          break;
        }
        case "Timeout":
          await vi.advanceTimersByTimeAsync(CHAT_SEND_DEADLINE_MS);
          expect(await submitted).toBeInstanceOf(Error);
          break;
        case "DropPayload":
          stores.chat = createChatStore();
          break;
        case "Retry":
          void delivery.retry(send.mock.calls[0][0].requestId!, applied);
          await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(2));
          break;
        default:
          throw new Error(`Unexpected client step ${step.action}`);
      }
    }
    const [draft] = loadChatDrafts().chatDrafts;
    const status = sendStatus(draft, Date.now());
    expect(JSON.parse(draft.requestJson)).toEqual(send.mock.calls[0][0]);
    if (trace.expect === "violates FailedOffersRetry") {
      expect(status).toBe("failed");
      expect(deliveryAffordances(status, false, false).retry).toBe(true);
    } else {
      expect(trace.expect).toBe("violates CreatedOnce");
      expect(committed).toBe(trace.constants.MaxSends);
      expect(ids.size).toBe(1);
    }
    reply.resolve(acceptedChat(send.mock.calls[0][0]));
    await vi.advanceTimersByTimeAsync(0);
  },
);
