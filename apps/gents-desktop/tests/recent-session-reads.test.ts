import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { BridgeInvokeError } from "@source-inc/gents-desktop-client";
import { startClientObservation } from "../src/hooks/clientObservation";
import { RECENT_READS, createSessionReads } from "../src/hooks/sessionReads";
import { createSelectionStore } from "../src/hooks/selectionStore";
import { createSessionStore, readSession } from "../src/hooks/sessionStore";
import { projectSessionLoadingStatus } from "../src/lib/loadingStatus";
import { node, testApp } from "./app-fixture";
import { assistantMessage, sessionSnapshot, userMessage } from "./timeline-fixture";

const NODE = "did:key:node-a";
const OTHER_NODE = "did:key:node-b";

/* a fresh object each call, equal for the same session and text */
const snapshotOf = (sessionId: string, nodeDid = NODE, reply = "hello") =>
  sessionSnapshot({
    sessionId,
    nodeDid,
    timelineItems: [
      userMessage({ itemKey: `${sessionId}-u`, requestId: "r-1", content: "hi" }),
      assistantMessage({ itemKey: `${sessionId}-a`, content: reply }),
    ],
  });

const denied = () =>
  new BridgeInvokeError({
    code: "permissionDenied",
    message: "Access denied",
    retryable: false,
    endpoint: null,
  });

type Pending = {
  sessionId: string;
  nodeDid: string | null;
  resolve: (value: DesktopSessionSnapshot | null) => void;
  reject: (error: unknown) => void;
};

/* the session reads with each fetch held until the test answers it */
function heldReads() {
  const pending: Pending[] = [];
  const fetchSessionSnapshot = vi.fn(
    (sessionId: string, nodeDid: string | null) =>
      new Promise<DesktopSessionSnapshot | null>((resolve, reject) => {
        pending.push({ sessionId, nodeDid, resolve, reject });
      }),
  );
  const store = createSelectionStore({ nodeDid: NODE, sessionId: null });
  const sessionStore = createSessionStore();
  const reads = createSessionReads({
    api: { fetchSessionSnapshot } as unknown as DesktopApiAdapter,
    store,
    sessionStore,
    trackedRequestId: () => null,
    setError: vi.fn(),
  });
  /* what the observer does on a selection change: show the kept read, then
     run the read it waits on */
  const open = (sessionId: string, nodeDid = NODE) => {
    store.setState({ sessionId, nodeDid });
    reads.showRecentRead();
    return reads.refreshSession(sessionId);
  };
  const answer = async (
    read: Promise<unknown>,
    value: DesktopSessionSnapshot | null,
  ) => {
    pending.at(-1)!.resolve(value);
    await read;
  };
  const refuse = async (read: Promise<unknown>) => {
    pending.at(-1)!.reject(denied());
    await read;
  };
  const status = () =>
    projectSessionLoadingStatus({
      selectedSessionId: store.getState().sessionId,
      selectedNodeDid: store.getState().nodeDid,
      session: readSession(sessionStore),
      sessionLoad: sessionStore.getState().load,
      operationalState: null,
    });
  return { reads, store, sessionStore, open, answer, refuse, status, pending };
}

/* reads each session once, in order, and leaves the last one open */
async function readAll(t: ReturnType<typeof heldReads>, sessionIds: string[]) {
  for (const sessionId of sessionIds)
    await t.answer(t.open(sessionId), snapshotOf(sessionId));
}

describe("recent session reads", () => {
  it("shows a reopened session at once from its last read, marked as updating", async () => {
    const t = heldReads();
    await readAll(t, ["s-1"]);
    const shown = readSession(t.sessionStore);
    await readAll(t, ["s-2"]);

    const read = t.open("s-1");
    expect(readSession(t.sessionStore)).toBe(shown);
    expect(t.sessionStore.getState().load).toMatchObject({
      phase: "cached",
      sessionId: "s-1",
      nodeDid: NODE,
      reread: true,
    });
    expect(t.status()).toMatchObject({ title: "Updating session", action: null });
    /* the read still runs */
    expect(t.pending.at(-1)?.sessionId).toBe("s-1");

    await t.answer(read, snapshotOf("s-1", NODE, "hello again"));
    expect(readSession(t.sessionStore)?.timelineItems.at(-1)).toMatchObject({
      content: "hello again",
    });
    expect(t.sessionStore.getState().load.phase).toBe("loaded");
    expect(t.status()).toBeNull();
  });

  it("renders nothing again when the read after it finds the session unchanged", async () => {
    const t = heldReads();
    await readAll(t, ["s-1", "s-2"]);
    const read = t.open("s-1");
    const shown = t.sessionStore.getState();
    const sessionWrites = vi.fn();
    const unsubscribe = t.sessionStore.subscribe((state, prev) => {
      if (state.session !== prev.session || state.facts !== prev.facts) sessionWrites();
    });

    await t.answer(read, snapshotOf("s-1"));
    unsubscribe();
    expect(sessionWrites).not.toHaveBeenCalled();
    expect(t.sessionStore.getState().session).toBe(shown.session);
    expect(t.sessionStore.getState().goal).toBe(shown.goal);
    expect(t.sessionStore.getState().load.phase).toBe("loaded");
  });

  it("clears a shown recent read that its read refuses, and forgets it", async () => {
    const t = heldReads();
    await readAll(t, ["s-1", "s-2"]);
    const read = t.open("s-1");
    expect(readSession(t.sessionStore)?.sessionId).toBe("s-1");

    await t.refuse(read);
    expect(readSession(t.sessionStore)).toBeNull();
    expect(t.sessionStore.getState().load).toMatchObject({ phase: "failed" });

    void t.open("s-2");
    expect(readSession(t.sessionStore)?.sessionId).toBe("s-2");
    void t.open("s-1");
    expect(readSession(t.sessionStore)?.sessionId).not.toBe("s-1");
    expect(t.sessionStore.getState().load.phase).toBe("loading");
  });

  it("forgets a session whose read is refused after the reader left it", async () => {
    const t = heldReads();
    await readAll(t, ["s-1", "s-2"]);
    const read = t.reads.refreshSession("s-2");
    const refused = t.pending.at(-1)!;
    void t.open("s-1");
    refused.reject(denied());
    await read;
    /* the session shown now is untouched */
    expect(readSession(t.sessionStore)?.sessionId).toBe("s-1");

    void t.open("s-2");
    expect(readSession(t.sessionStore)?.sessionId).not.toBe("s-2");
  });

  it("clears a shown recent read whose read fails, as if it had not been shown", async () => {
    const t = heldReads();
    await readAll(t, ["s-1", "s-2"]);
    const read = t.open("s-1");
    t.pending.at(-1)!.reject(new Error("read failed"));
    await read;
    expect(readSession(t.sessionStore)).toBeNull();
    expect(t.status()).toMatchObject({ phase: "failed", action: "retryLocal" });
  });

  it(`keeps only the last ${RECENT_READS} sessions read`, async () => {
    const t = heldReads();
    const ids = Array.from({ length: RECENT_READS + 1 }, (_, i) => `s-${i}`);
    await readAll(t, ids);

    void t.open("s-0");
    expect(t.sessionStore.getState().load.phase).toBe("loading");
    expect(readSession(t.sessionStore)?.sessionId).not.toBe("s-0");
    for (const sessionId of ids.slice(1)) {
      void t.open(sessionId);
      expect(t.sessionStore.getState().load.phase).toBe("cached");
      expect(readSession(t.sessionStore)?.sessionId).toBe(sessionId);
    }
  });

  it("never shows a kept read under another node or session", async () => {
    const t = heldReads();
    await readAll(t, ["s-1"]);
    await t.answer(t.open("s-other"), snapshotOf("s-other"));

    void t.open("s-1", OTHER_NODE);
    expect(readSession(t.sessionStore)).not.toMatchObject({ sessionId: "s-1" });
    expect(t.sessionStore.getState().load).toMatchObject({
      phase: "loading",
      nodeDid: OTHER_NODE,
    });

    void t.open("s-2");
    expect(readSession(t.sessionStore)).not.toMatchObject({ sessionId: "s-1" });
    expect(t.sessionStore.getState().load.phase).toBe("loading");

    /* the read that lands for a session the reader left writes nothing */
    const left = t.open("s-1");
    const leftRead = t.pending.at(-1)!;
    void t.open("s-other");
    leftRead.resolve(snapshotOf("s-1", NODE, "late"));
    await left;
    expect(readSession(t.sessionStore)?.sessionId).toBe("s-other");
  });

  it("forgets every kept read when the client identity changes, and a node's when it is removed", async () => {
    const t = heldReads();
    const client = (clientDid: string, nodeDids: string[]) =>
      ({
        clientDid,
        deployments: nodeDids.map((nodeDid) => ({ nodeDid })),
        endedAccess: [],
      }) as unknown as DesktopClientSnapshot["client"];
    t.reads.forgetWithdrawnReads(client("did:key:me", [NODE, OTHER_NODE]));
    await readAll(t, ["s-1"]);
    await t.answer(t.open("s-2", OTHER_NODE), snapshotOf("s-2", OTHER_NODE));

    t.reads.forgetWithdrawnReads(client("did:key:me", [OTHER_NODE]));
    void t.open("s-1");
    expect(t.sessionStore.getState().load.phase).toBe("loading");
    void t.open("s-2", OTHER_NODE);
    expect(t.sessionStore.getState().load.phase).toBe("cached");

    t.reads.forgetWithdrawnReads(client("did:key:someone-else", [OTHER_NODE]));
    void t.open("s-1");
    void t.open("s-2", OTHER_NODE);
    expect(t.sessionStore.getState().load.phase).toBe("loading");
  });
});

describe("opening a recently read session in the app", () => {
  afterEach(() => vi.useRealTimers());

  it("puts its transcript on screen in the same update as the selection", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    const running = {
      bootstrap: { clientStateExists: true, savedPeers: [] },
      client: {
        deployments: [
          node({
            nodeDid: NODE,
            sessions: ["s-1", "s-2"].map((sessionId) => ({
              sessionId,
              nodeDid: NODE,
              requesterDid: null,
            })),
          }),
        ],
      },
    } as unknown as DesktopClientSnapshot;
    const pending: ((value: DesktopSessionSnapshot) => void)[] = [];
    const api = new Proxy(
      {
        fetchDesktopSnapshot: vi.fn(async () => running),
        fetchSessionSnapshot: vi.fn(
          () => new Promise<DesktopSessionSnapshot>((resolve) => pending.push(resolve)),
        ),
      } as Record<string, unknown>,
      {
        get: (target, key: string) =>
          target[key] ?? (target[key] = vi.fn(async () => null)),
      },
    );
    const app = testApp({
      api,
      snapshot: running,
      selection: { nodeDid: NODE, sessionId: "s-1" },
    });
    const stop = startClientObservation(
      app,
      vi.fn(async () => () => {}),
    );
    try {
      await vi.advanceTimersByTimeAsync(0);
      pending.shift()!(snapshotOf("s-1"));
      await vi.advanceTimersByTimeAsync(0);
      const first = readSession(app.stores.session);
      expect(first?.sessionId).toBe("s-1");

      app.actions.selectSession("s-2");
      await vi.advanceTimersByTimeAsync(0);
      pending.shift()!(snapshotOf("s-2"));
      await vi.advanceTimersByTimeAsync(0);
      expect(readSession(app.stores.session)?.sessionId).toBe("s-2");

      app.actions.selectSession("s-1");
      expect(readSession(app.stores.session)).toBe(first);
      expect(app.view.getState().loadingStatus).toMatchObject({
        title: "Updating session",
      });
    } finally {
      stop();
    }
  });
});
