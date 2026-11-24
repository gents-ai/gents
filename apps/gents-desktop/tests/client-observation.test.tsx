import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopClientSnapshot,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";

import { useClientRuntime } from "../src/hooks/useClientRuntime";
import {
  UNREGISTERED_READ_MS,
  startClientObservation,
} from "../src/hooks/clientObservation";
import * as shellPlatform from "../src/lib/shellPlatform";
import { listedSession } from "../src/hooks/fleetStore";
import { node, testApp } from "./app-fixture";
import { liveAssistant, sessionSnapshot } from "./timeline-fixture";

const NODE_DID = "did:key:here";
const running = {
  bootstrap: { clientStateExists: true, savedRuntimes: [] },
  client: {
    deployments: [
      node({
        nodeDid: NODE_DID,
        sessions: [
          { sessionId: "s-1", nodeDid: NODE_DID, requesterDid: null },
          { sessionId: "s-2", nodeDid: NODE_DID, requesterDid: null },
        ],
      }),
    ],
  },
} as unknown as DesktopClientSnapshot;

function bridgeApi(): Record<string, ReturnType<typeof vi.fn>> {
  return new Proxy(
    { fetchDesktopSnapshot: vi.fn(async () => running) },
    {
      get: (target, key: string) =>
        (target as Record<string, unknown>)[key] ??
        ((target as Record<string, unknown>)[key] = vi.fn(async () => null)),
    },
  );
}

function showWindow(shown: boolean) {
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => (shown ? "visible" : "hidden"),
  });
  document.dispatchEvent(new Event("visibilitychange"));
}

/* reads requested in the same moment join one read after a short burst
   window (projectionController's BURST_MS) */
const BURST = 150;

describe("following the client while it runs", () => {
  it.each([false, true])(
    "reads the open session on store updates and never on a timer (live source %s)",
    async (hasLive) => {
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
      let stop: (() => void) | undefined;
      try {
        const api = bridgeApi();
        const session = sessionSnapshot({
          sessionId: "s-1",
          nodeDid: NODE_DID,
          turnState: "running",
          latestRequestId: "r-1",
          liveCursor: hasLive ? "source" : null,
          projectionRevision: { storeVersion: 1, provenanceVersion: 1 },
          timelineItems: hasLive
            ? [liveAssistant({ itemKey: "live", content: "hello" })]
            : [],
        });
        api.fetchSessionSnapshot.mockResolvedValue(session);
        api.fetchSessionLiveDelta.mockResolvedValue({
          outcome: "unchanged",
          liveCursor: "source",
          requestId: "r-1",
          revision: { storeVersion: 1, provenanceVersion: 1 },
          turnState: "running",
          status: null,
          content: { mode: "unchanged", value: "", byteLen: 5, hash: "4f9f2cab" },
          reasoning: { mode: "unchanged", value: "", byteLen: 0, hash: "811c9dc5" },
        });
        const app = testApp({ api, snapshot: running, session });
        expect(app.trackedRequestId()).toBe("r-1");
        let update: ((event: { reason?: string }) => Promise<void>) | undefined;
        stop = startClientObservation(
          app,
          vi.fn(async (listener) => {
            update = listener as typeof update;
            return () => {};
          }),
        );
        await vi.advanceTimersByTimeAsync(BURST);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
        await vi.advanceTimersByTimeAsync(10_000);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
        expect(api.fetchSessionLiveDelta).not.toHaveBeenCalled();

        void update?.({ reason: "store" });
        await vi.advanceTimersByTimeAsync(BURST);
        expect(api.fetchSessionLiveDelta).toHaveBeenCalledTimes(0);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(2);

        void update?.({ reason: "store" });
        await vi.advanceTimersByTimeAsync(BURST);
        expect(api.fetchSessionLiveDelta).toHaveBeenCalledTimes(hasLive ? 1 : 0);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(hasLive ? 2 : 3);
        /* the live read stood in for the update; the full read it owes
           follows once the read gap ends */
        await vi.advanceTimersByTimeAsync(1_500);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(3);
        await vi.advanceTimersByTimeAsync(10_000);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(3);
        stop();
        await vi.advanceTimersByTimeAsync(3_000);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(3);
      } finally {
        stop?.();
        vi.useRealTimers();
      }
    },
  );

  it("shows another session's change in the list while the open session's turn is followed", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    let stop: (() => void) | undefined;
    try {
      const api = bridgeApi();
      /* s-1 is open, its turn followed; s-2 is another row in the list */
      const listed = (otherTurn: string) =>
        ({
          ...running,
          client: {
            deployments: [
              node({
                nodeDid: NODE_DID,
                sessions: [
                  { sessionId: "s-1", nodeDid: NODE_DID, requesterDid: null },
                  {
                    sessionId: "s-2",
                    nodeDid: NODE_DID,
                    requesterDid: null,
                    turnState: otherTurn,
                  },
                ],
              }),
            ],
          },
        }) as unknown as DesktopClientSnapshot;
      api.fetchDesktopSnapshot.mockResolvedValue(listed("idle"));
      const session = sessionSnapshot({
        sessionId: "s-1",
        nodeDid: NODE_DID,
        turnState: "running",
        latestRequestId: "r-1",
        liveCursor: null,
        projectionRevision: { storeVersion: 1, provenanceVersion: 1 },
        timelineItems: [],
      });
      api.fetchSessionSnapshot.mockResolvedValue(session);
      const app = testApp({ api, snapshot: listed("idle"), session });
      expect(app.trackedRequestId()).toBe("r-1");
      let update:
        | ((event: { reason?: string; sessionList?: string }) => Promise<void>)
        | undefined;
      stop = startClientObservation(
        app,
        vi.fn(async (listener) => {
          update = listener as typeof update;
          return () => {};
        }),
      );
      await vi.advanceTimersByTimeAsync(BURST);
      const otherTurn = () =>
        listedSession(app.stores.fleet.getState(), NODE_DID, "s-2")?.turnState;
      expect(otherTurn()).toBe("idle");

      /* s-2 starts a turn of its own; its store update arrives while s-1's
         turn is still followed */
      api.fetchDesktopSnapshot.mockResolvedValue(listed("running"));
      void update?.({ reason: "store", sessionList: "list-2" });
      await vi.advanceTimersByTimeAsync(BURST);
      expect(app.trackedRequestId()).toBe("r-1");
      expect(otherTurn()).toBe("running");
    } finally {
      stop?.();
      vi.useRealTimers();
    }
  });

  it("reads no list for the followed turn's reply pieces", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    let stop: (() => void) | undefined;
    try {
      const api = bridgeApi();
      const session = sessionSnapshot({
        sessionId: "s-1",
        nodeDid: NODE_DID,
        turnState: "running",
        latestRequestId: "r-1",
        liveCursor: null,
        projectionRevision: { storeVersion: 1, provenanceVersion: 1 },
        timelineItems: [],
      });
      api.fetchSessionSnapshot.mockResolvedValue(session);
      const app = testApp({ api, snapshot: running, session });
      let update:
        | ((event: { reason?: string; sessionList?: string }) => Promise<void>)
        | undefined;
      stop = startClientObservation(
        app,
        vi.fn(async (listener) => {
          update = listener as typeof update;
          return () => {};
        }),
      );
      await vi.advanceTimersByTimeAsync(BURST);
      /* the first store update's list is new to the client */
      void update?.({ reason: "store", sessionList: "list-1" });
      await vi.advanceTimersByTimeAsync(BURST);
      const listReads = api.fetchDesktopSnapshot.mock.calls.length;

      /* reply pieces: store updates that change no row the list shows */
      for (let piece = 0; piece < 5; piece++) {
        void update?.({ reason: "store", sessionList: "list-1" });
        await vi.advanceTimersByTimeAsync(BURST);
      }
      expect(app.trackedRequestId()).toBe("r-1");
      expect(api.fetchDesktopSnapshot).toHaveBeenCalledTimes(listReads);
      expect(api.fetchSessionSnapshot.mock.calls.length).toBeGreaterThan(1);

      /* an update without a fingerprint cannot say a row changed: it reads
         the open session only, as on main */
      for (let piece = 0; piece < 3; piece++) {
        void update?.({ reason: "store" });
        await vi.advanceTimersByTimeAsync(BURST);
      }
      expect(api.fetchDesktopSnapshot).toHaveBeenCalledTimes(listReads);

      /* a row changes: the list is read once */
      void update?.({ reason: "store", sessionList: "list-2" });
      await vi.advanceTimersByTimeAsync(BURST);
      expect(api.fetchDesktopSnapshot).toHaveBeenCalledTimes(listReads + 1);
    } finally {
      stop?.();
      vi.useRealTimers();
    }
  });

  it("reads nothing while the window is hidden, and reads in full on show", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    let stop: (() => void) | undefined;
    try {
      const api = bridgeApi();
      const session = sessionSnapshot({
        sessionId: "s-1",
        nodeDid: NODE_DID,
        turnState: "running",
        latestRequestId: "r-1",
        liveCursor: "source",
        projectionRevision: { storeVersion: 1, provenanceVersion: 1 },
        timelineItems: [liveAssistant({ itemKey: "live", content: "hello" })],
      });
      api.fetchSessionSnapshot.mockResolvedValue(session);
      const app = testApp({ api, snapshot: running, session });
      stop = startClientObservation(
        app,
        vi.fn(async () => () => {}),
      );
      await vi.advanceTimersByTimeAsync(BURST);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
      const indexReads = api.fetchDesktopSnapshot.mock.calls.length;

      showWindow(false);
      await vi.advanceTimersByTimeAsync(60_000);
      expect(api.fetchSessionLiveDelta).not.toHaveBeenCalled();
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
      expect(api.fetchDesktopSnapshot).toHaveBeenCalledTimes(indexReads);
      expect(api.reconnectNode).not.toHaveBeenCalled();

      showWindow(true);
      await vi.advanceTimersByTimeAsync(BURST);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(2);
      expect(api.fetchDesktopSnapshot).toHaveBeenCalledTimes(indexReads + 1);

      expect(api.reconnectNode.mock.calls).toEqual([[null]]);

      await vi.advanceTimersByTimeAsync(60_000);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(2);
      expect(api.fetchSessionLiveDelta).not.toHaveBeenCalled();
    } finally {
      stop?.();
      Reflect.deleteProperty(document, "visibilityState");
      vi.useRealTimers();
    }
  });

  it("asks every node to reconnect now when the device comes online or its network changes", async () => {
    let stop: (() => void) | undefined;
    const connection = new EventTarget();
    Object.defineProperty(navigator, "connection", {
      configurable: true,
      get: () => connection,
    });
    vi.useFakeTimers();
    try {
      const api = bridgeApi();
      const app = testApp({ api, snapshot: running });
      stop = startClientObservation(
        app,
        vi.fn(async () => () => {}),
      );
      await vi.advanceTimersByTimeAsync(BURST);
      expect(api.reconnectNode).not.toHaveBeenCalled();

      window.dispatchEvent(new Event("online"));
      expect(api.reconnectNode.mock.calls).toEqual([[null]]);

      connection.dispatchEvent(new Event("change"));
      expect(api.reconnectNode.mock.calls).toEqual([[null], [null]]);

      /* the observer's retry now leaves a connected node alone, so asking
         for every node restarts only those still lost */
      window.dispatchEvent(new Event("focus"));
      expect(api.reconnectNode.mock.calls).toHaveLength(2);
    } finally {
      stop?.();
      Reflect.deleteProperty(navigator, "connection");
      vi.useRealTimers();
    }
  });

  it("keeps one bridge listener while the person moves between sessions", async () => {
    const listen = vi.fn<DesktopClientUpdatedListenerFactory>(async () => () => {});
    const app = testApp({
      api: bridgeApi(),
      snapshot: running,
      selection: { nodeDid: NODE_DID, sessionId: "s-1" },
    });
    renderHook(() => useClientRuntime(app, listen));
    await waitFor(() => expect(listen).toHaveBeenCalledOnce());

    act(() => app.actions.selectSession("s-2"));
    act(() => app.actions.selectSession("s-1"));
    await act(async () => {});

    expect(listen).toHaveBeenCalledOnce();
  });

  it("reads the session moved to and tells the host the node moved to", async () => {
    const api = bridgeApi();
    const app = testApp({
      api,
      snapshot: {
        ...running,
        client: {
          deployments: [
            ...running.client!.deployments,
            node({ nodeDid: "did:key:there" }),
          ],
        },
      },
      selection: { nodeDid: NODE_DID, sessionId: "s-1" },
    });
    renderHook(() =>
      useClientRuntime(
        app,
        vi.fn(async () => () => {}),
      ),
    );
    await waitFor(() => expect(api.setSelectedNode).toHaveBeenCalledWith(NODE_DID));

    act(() => app.actions.selectSession("s-2"));
    await waitFor(() =>
      expect(api.fetchSessionSnapshot).toHaveBeenCalledWith(
        "s-2",
        NODE_DID,
        null,
        expect.anything(),
      ),
    );

    act(() => app.actions.selectNode("did:key:there"));
    await waitFor(() =>
      expect(api.setSelectedNode).toHaveBeenLastCalledWith("did:key:there"),
    );
  });

  it("selects the first node within the read that lists it, so the host never hears of none", async () => {
    const api = bridgeApi();
    const app = testApp({ api, snapshot: running });
    renderHook(() =>
      useClientRuntime(
        app,
        vi.fn(async () => () => {}),
      ),
    );
    await waitFor(() => expect(api.setSelectedNode).toHaveBeenCalled());
    await act(async () => {});

    expect(api.setSelectedNode.mock.calls).toEqual([[NODE_DID]]);
  });

  it("refreshes on native window focus without publishing selection again", async () => {
    const mac = vi.spyOn(shellPlatform, "isMacTauriShell").mockReturnValue(true);
    let stop: (() => void) | undefined;
    try {
      const api = bridgeApi();
      const app = testApp({ api, snapshot: running });
      stop = startClientObservation(
        app,
        vi.fn(async () => () => {}),
      );
      await act(async () => {});
      expect(api.setSelectedNode.mock.calls).toEqual([[NODE_DID]]);

      const reads = api.fetchDesktopSnapshot.mock.calls.length;
      await act(async () => {
        window.dispatchEvent(new Event("focus"));
      });
      await waitFor(() =>
        expect(api.fetchDesktopSnapshot).toHaveBeenCalledTimes(reads + 1),
      );
      expect(api.setSelectedNode.mock.calls).toEqual([[NODE_DID]]);

      stop();
      await act(async () => {
        window.dispatchEvent(new Event("focus"));
      });
      expect(api.fetchDesktopSnapshot).toHaveBeenCalledTimes(reads + 1);
      expect(api.setSelectedNode.mock.calls).toEqual([[NODE_DID]]);
    } finally {
      stop?.();
      mac.mockRestore();
    }
  });
});

describe("registering for client updates", () => {
  it("reads the selection by a deadline while the listener is not registered, then in full once it is", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    let stop: (() => void) | undefined;
    try {
      const api = bridgeApi();
      const app = testApp({ api, snapshot: running });
      app.stores.selection.setState({ nodeDid: NODE_DID, sessionId: "s-1" });
      let register!: () => void;
      const registered = new Promise<void>((resolve) => {
        register = resolve;
      });
      stop = startClientObservation(
        app,
        vi.fn(async () => {
          await registered;
          return () => {};
        }),
      );
      await vi.advanceTimersByTimeAsync(UNREGISTERED_READ_MS - 1);
      expect(api.fetchSessionSnapshot).not.toHaveBeenCalled();
      /* a registration that never settles must not leave the selection
         unread; the read then waits out the burst window */
      await vi.advanceTimersByTimeAsync(1 + BURST);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(60_000);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
      /* that read may have missed a change Tauri dropped before
         registration, so the registration reads in full again */
      const snapshotsBefore = api.fetchDesktopSnapshot.mock.calls.length;
      register();
      await vi.advanceTimersByTimeAsync(BURST);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(2);
      expect(api.fetchDesktopSnapshot.mock.calls.length).toBe(snapshotsBefore + 1);
    } finally {
      stop?.();
      vi.useRealTimers();
    }
  });

  it("reads the selection when registration rejects, re-listens, then reads in full", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    let stop: (() => void) | undefined;
    try {
      const api = bridgeApi();
      const app = testApp({ api, snapshot: running });
      app.stores.selection.setState({ nodeDid: NODE_DID, sessionId: "s-1" });
      let attempts = 0;
      let update: ((event: { reason?: string }) => Promise<void>) | undefined;
      stop = startClientObservation(
        app,
        vi.fn(async (listener) => {
          attempts += 1;
          if (attempts <= 2) throw new Error("listen refused");
          update = listener as typeof update;
          return () => {};
        }),
      );
      await vi.advanceTimersByTimeAsync(0);
      expect(attempts).toBe(1);
      expect(app.stores.client.getState().error).toBe("listen refused");
      await vi.advanceTimersByTimeAsync(BURST);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(250 - BURST);
      expect(attempts).toBe(2);
      await vi.advanceTimersByTimeAsync(499);
      expect(attempts).toBe(2);
      await vi.advanceTimersByTimeAsync(1);
      expect(attempts).toBe(3);
      expect(app.stores.client.getState().error).toBeNull();
      await vi.advanceTimersByTimeAsync(BURST);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(2);
      void update?.({ reason: "store" });
      await vi.advanceTimersByTimeAsync(BURST);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(3);
    } finally {
      stop?.();
      vi.useRealTimers();
    }
  });
});
