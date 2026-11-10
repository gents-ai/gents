import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  ChatSendResult,
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

import {
  CHAT_SEND_DEADLINE_MS,
  createChatDelivery,
  observeLandedChatSends,
} from "../../src/hooks/chatDelivery";
import { writeSession } from "../../src/hooks/sessionStore";
import { keepSendDeadlineClock } from "../../src/hooks/sendDeadlineClock";
import { loadChatDrafts } from "../../src/hooks/chatDrafts";
import {
  deliveryAffordances,
  sendStatus,
  type DeliveryAffordances,
  type SendStatus,
} from "../../src/hooks/chatDeliveryView";
import { acceptedChat } from "../chat-delivery-fixture";
import { goalDeliveryStorage } from "../goal-delivery-storage";
import { shellStores } from "../shell-fixture";
import { memorySendAttachments } from "../send-attachments-fixture";

type ViewCase = Omit<DeliveryAffordances, "view"> & {
  status: SendStatus;
  mailbox: boolean;
  view: DeliveryAffordances["view"];
};
type Snapshot = {
  status: SendStatus | null;
  payload: boolean;
  turns: number;
  wire: number;
  retryOffered: boolean;
};
type Action = { kind: string; ok?: boolean };
type Trace = { actions: Action[]; after: Snapshot[] };
const run = promisify(execFile);

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

async function leanCases() {
  const cwd = join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../crates/gents/proofs",
  );
  await run("lake", ["build", "Proofs.Conformance.ClientOptimisticSend"], {
    cwd,
    timeout: 240_000,
  });
  const { stdout } = await run(
    "lake",
    ["env", "lean", "--run", "Proofs/Conformance/ClientOptimisticSend.lean"],
    { cwd, timeout: 240_000, maxBuffer: 64 * 1024 * 1024 },
  );
  return JSON.parse(stdout) as { views: ViewCase[]; traces: Trace[] };
}

const flush = async () => {
  for (let i = 0; i < 20; i++) await Promise.resolve();
};

/** Drives `createChatDelivery` through one Lean trace. The runtime and the
 * transport are the trace's: each create the client dispatches waits on the
 * wire until the trace commits, refuses or drops it, and each reply waits
 * until the trace delivers or loses it. The client's state is only what the
 * code saved. */
async function replay(trace: Trace, mailbox: boolean) {
  vi.useFakeTimers();
  vi.stubGlobal("localStorage", goalDeliveryStorage());
  const stores = shellStores({
    deployments: [{ nodeDid: "node", sessions: [], mailboxItems: [] }],
    selection: { nodeDid: "node", sessionId: "session" },
  });
  stores.client.setState({
    snapshot: { client: { clientDid: "client" } } as DesktopClientSnapshot,
  });
  type Dispatched = {
    request: ChatSendRequest;
    resolve: (result: ChatSendResult) => void;
    reject: (error: unknown) => void;
  };
  const wire: Dispatched[] = [];
  const replies: { ok: boolean; deliver: () => void }[] = [];
  const api = {
    sendChatMessage: (request: ChatSendRequest) =>
      new Promise<ChatSendResult>((resolve, reject) =>
        wire.push({ request, resolve, reject }),
      ),
  } as unknown as DesktopApiAdapter;
  const delivery = createChatDelivery(api, stores, memorySendAttachments());
  const stopClock = keepSendDeadlineClock(stores.chat);
  const stopObserving = observeLandedChatSends(stores);
  const applied = async () => true;
  let requestId: string | null = null;
  let discarded = false;
  const take = (ok: boolean) => {
    const index = replies.findIndex((reply) => reply.ok === ok);
    expect(index, "the trace's reply is on its way").toBeGreaterThanOrEqual(0);
    return replies.splice(index, 1)[0];
  };
  const observed = (): Omit<Snapshot, "turns"> => {
    const draft = loadChatDrafts().chatDrafts.find(
      (row) => row.requestId === requestId,
    );
    const status: SendStatus | null = draft
      ? sendStatus(draft, Date.now())
      : requestId === null
        ? null
        : discarded
          ? "discarded"
          : "confirmed";
    return {
      status,
      payload: Boolean(draft && !draft.confirmed),
      wire: wire.length,
      retryOffered: status !== null && deliveryAffordances(status, mailbox).retry,
    };
  };
  for (const [index, action] of trace.actions.entries()) {
    switch (action.kind) {
      case "compose":
        void delivery
          .submit(
            {
              clientDid: "client",
              nodeDid: "node",
              agentDid: "agent",
              sessionId: "session",
              content: "authored",
              attachments: [],
              delivery: null,
              causedBySourceDocId: mailbox ? "item" : null,
              cwd: null,
            },
            "session",
            false,
            applied,
            mailbox
              ? { nodeDid: "node", itemId: "item", summary: "authored" }
              : undefined,
          )
          .catch(() => {});
        await flush();
        requestId = loadChatDrafts().chatDrafts[0]?.requestId ?? null;
        break;
      case "commit": {
        const create = wire.shift()!;
        replies.push({
          ok: true,
          deliver: () => create.resolve(acceptedChat(create.request)),
        });
        break;
      }
      case "refuse":
      case "drop": {
        const create = wire.shift()!;
        replies.push({
          ok: false,
          deliver: () => create.reject(new Error(action.kind)),
        });
        break;
      }
      case "loseReply":
        take(action.ok!);
        break;
      case "receive":
        take(action.ok!).deliver();
        break;
      case "timeout":
        await vi.advanceTimersByTimeAsync(CHAT_SEND_DEADLINE_MS);
        break;
      case "retry":
        void delivery.retry(requestId!, applied).catch(() => {});
        break;
      case "discard":
        await delivery.discard(requestId!);
        discarded = true;
        break;
      case "observe":
        writeSession(stores.session, {
          sessionId: "session",
          nodeDid: "node",
          latestRequestId: requestId,
          pendingTurn: null,
          queuedTurns: [],
          foldedInputs: [],
          timelineItems: [],
        } as unknown as DesktopSessionSnapshot);
        break;
      default:
        throw new Error(`unmodeled action ${action.kind}`);
    }
    await flush();
    const { turns: _, ...expected } = trace.after[index];
    expect(observed(), JSON.stringify(trace.actions.slice(0, index + 1))).toEqual(
      expected,
    );
    if (action.kind === "compose" || action.kind === "retry")
      expect(wire.at(-1)?.request.requestId).toBe(requestId);
  }
  stopObserving();
  stopClock();
  vi.useRealTimers();
}

it("derives the send view and actions from the Lean optimistic send owner", async () => {
  const { views } = await leanCases();
  expect(views).toHaveLength(8);
  for (const { status, mailbox, ...expected } of views)
    expect(
      deliveryAffordances(status, mailbox),
      `${status} mailbox=${mailbox}`,
    ).toEqual(expected);
}, 300_000);

it("runs chatDelivery through every Lean optimistic send trace", async () => {
  const { traces } = await leanCases();
  expect(traces.length).toBeGreaterThan(100);
  const kinds = new Set(traces.flatMap((trace) => trace.actions.map((a) => a.kind)));
  expect([...kinds].sort()).toEqual(
    [
      "commit",
      "compose",
      "discard",
      "drop",
      "loseReply",
      "observe",
      "receive",
      "refuse",
      "retry",
      "timeout",
    ].sort(),
  );
  for (const trace of traces)
    for (const mailbox of [false, true]) await replay(trace, mailbox);
}, 900_000);
