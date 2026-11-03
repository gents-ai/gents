import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  BridgeInvokeError,
  type DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import type { BridgeErrorCode } from "@source-inc/gents-desktop-client/generated/BridgeErrorCode";

import { startClientObservation } from "../src/hooks/clientObservation";
import { LoadingStatus } from "../src/ui/screens/LoadingStatus";
import { node, renderIn, testApp } from "./app-fixture";
import { sessionSnapshot } from "./timeline-fixture";

const NODE_DID = "did:key:here";
const running = {
  bootstrap: { clientStateExists: true, savedPeers: [] },
  client: {
    deployments: [
      node({
        nodeDid: NODE_DID,
        sessions: [{ sessionId: "s-1", nodeDid: NODE_DID, requesterDid: null }],
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

type Listener = (event: { reason?: string }) => Promise<void>;

/* a session with a request that has not finished and no live text, whose
   read takes `readMs` and, while `changing` says so, differs each time */
function observe({ readMs, changing }: { readMs: number; changing: () => boolean }) {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
  const api = bridgeApi();
  const session = sessionSnapshot({
    sessionId: "s-1",
    nodeDid: NODE_DID,
    turnState: "running",
    latestRequestId: "r-1",
    liveCursor: null,
    timelineItems: [],
  });
  const started: number[] = [];
  let edits = 0;
  api.fetchSessionSnapshot.mockImplementation(() => {
    started.push(performance.now());
    const next = changing() ? { ...session, title: `edit ${++edits}` } : session;
    return new Promise((resolve) => setTimeout(() => resolve(next), readMs));
  });
  const app = testApp({ api, snapshot: running, session });
  let update: Listener | undefined;
  const stop = startClientObservation(
    app,
    vi.fn(async (listener) => {
      update = listener as Listener;
      return () => {};
    }),
  );
  const sendUpdate = async (reason = "store") => {
    void update?.({ reason });
    await vi.advanceTimersByTimeAsync(0);
  };
  return { app, api, stop, started, sendUpdate };
}

function deliveryError(code: BridgeErrorCode) {
  return new BridgeInvokeError({
    code,
    message: `${code} on Studio`,
    retryable: true,
    endpoint: NODE_DID,
  });
}

afterEach(() => vi.useRealTimers());

describe("reading a session with an open request", () => {
  it("reads nothing while no update arrives", async () => {
    const run = observe({ readMs: 0, changing: () => true });
    try {
      await vi.advanceTimersByTimeAsync(0);
      expect(run.started).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(120_000);
      expect(run.started).toHaveLength(1);
    } finally {
      run.stop();
    }
  });

  it("reads an update once the last read has finished, never back to back and never dropped", async () => {
    const run = observe({ readMs: 1_000, changing: () => true });
    try {
      await vi.advanceTimersByTimeAsync(1_000);
      expect(run.started).toHaveLength(1);
      await run.sendUpdate();
      await vi.advanceTimersByTimeAsync(500);
      await run.sendUpdate();
      expect(run.started).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(499);
      expect(run.started).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(1);
      expect(run.started).toHaveLength(2);
      await vi.advanceTimersByTimeAsync(60_000);
      expect(run.started).toHaveLength(2);
    } finally {
      run.stop();
    }
  });

  it("reads on an update after a quiet stretch shorter than a minute", async () => {
    const run = observe({ readMs: 0, changing: () => false });
    try {
      await vi.advanceTimersByTimeAsync(30_200);
      const before = run.started.length;
      await run.sendUpdate();
      expect(run.started).toHaveLength(before + 1);
    } finally {
      run.stop();
    }
  });

  it("reads updates at the slower pace once the session has gone quiet, but still reads them", async () => {
    const run = observe({ readMs: 0, changing: () => false });
    try {
      await vi.advanceTimersByTimeAsync(61_000);
      await run.sendUpdate();
      const before = run.started.length;
      for (let i = 0; i < 20; i++) {
        await vi.advanceTimersByTimeAsync(250);
        await run.sendUpdate();
      }
      expect(run.started).toHaveLength(before);
      await vi.advanceTimersByTimeAsync(3_000);
      expect(run.started).toHaveLength(before + 1);
      await vi.advanceTimersByTimeAsync(60_000);
      expect(run.started).toHaveLength(before + 1);
    } finally {
      run.stop();
    }
  });
});

describe("a session waiting on its node", () => {
  it("re-reads when the pairing changes and when the store changes, never on a timer", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    const api = bridgeApi();
    const reads = [
      deliveryError("sessionNotConnected"),
      deliveryError("sessionDelivering"),
      deliveryError("sessionAccepted"),
    ];
    const visible = sessionSnapshot({
      sessionId: "s-1",
      nodeDid: NODE_DID,
      turnState: "running",
      latestRequestId: "r-1",
      liveCursor: null,
      timelineItems: [],
    });
    api.fetchSessionSnapshot.mockImplementation(async () => {
      const next = reads.shift();
      if (next) throw next;
      return visible;
    });
    const app = testApp({
      api,
      snapshot: running,
      selection: { nodeDid: NODE_DID, sessionId: "s-1" },
    });
    app.stores.chat.setState({
      localWorkflow: {
        kind: "awaitingObservation",
        nodeDid: NODE_DID,
        sessionId: "s-1",
        requestId: "r-1",
      },
    });
    let update: Listener | undefined;
    const stop = startClientObservation(
      app,
      vi.fn(async (listener) => {
        update = listener as Listener;
        return () => {};
      }),
    );
    const send = async (reason: string) => {
      void update?.({ reason });
      await vi.advanceTimersByTimeAsync(0);
    };
    const phase = () => app.stores.session.getState().load.phase;
    try {
      await vi.advanceTimersByTimeAsync(0);
      expect(phase()).toBe("notConnected");
      expect(app.view.getState().loadingStatus).toMatchObject({
        phase: "blocked",
        title: "Not connected to the node",
      });
      await vi.advanceTimersByTimeAsync(60_000);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);

      await send("health");
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(2);
      expect(phase()).toBe("delivering");

      await send("store");
      await vi.advanceTimersByTimeAsync(0);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(3);
      expect(phase()).toBe("accepted");

      await send("store");
      await vi.advanceTimersByTimeAsync(0);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(4);
      expect(phase()).toBe("loaded");

      await send("health");
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(4);
    } finally {
      stop();
    }
  });

  it("re-reads after a pairing change that lands while the first read is in flight", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    const api = bridgeApi();
    let land!: () => void;
    api.fetchSessionSnapshot
      .mockImplementationOnce(
        () =>
          new Promise((_resolve, reject) => {
            land = () => reject(deliveryError("sessionNotConnected"));
          }),
      )
      .mockRejectedValueOnce(deliveryError("sessionDelivering"));
    const app = testApp({
      api,
      snapshot: running,
      selection: { nodeDid: NODE_DID, sessionId: "s-1" },
    });
    app.stores.chat.setState({
      localWorkflow: {
        kind: "awaitingObservation",
        nodeDid: NODE_DID,
        sessionId: "s-1",
        requestId: "r-1",
      },
    });
    let update: Listener | undefined;
    const stop = startClientObservation(
      app,
      vi.fn(async (listener) => {
        update = listener as Listener;
        return () => {};
      }),
    );
    try {
      await vi.advanceTimersByTimeAsync(0);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
      expect(app.stores.session.getState().load.phase).toBe("loading");
      void update?.({ reason: "health" });
      await vi.advanceTimersByTimeAsync(0);
      land();
      await vi.advanceTimersByTimeAsync(0);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(2);
      expect(app.stores.session.getState().load.phase).toBe("delivering");
    } finally {
      stop();
    }
  });

  it("waits for the update that brings a pending request rather than polling for it", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    const api = bridgeApi();
    api.fetchSessionSnapshot.mockImplementation(async () => {
      throw new BridgeInvokeError({
        code: "projectionPending",
        message: "selected request has not reached the session owner yet",
        retryable: true,
        endpoint: null,
      });
    });
    const app = testApp({
      api,
      snapshot: running,
      selection: { nodeDid: NODE_DID, sessionId: "s-1" },
    });
    app.stores.chat.setState({
      localWorkflow: {
        kind: "awaitingObservation",
        nodeDid: NODE_DID,
        sessionId: "s-1",
        requestId: "r-1",
      },
    });
    expect(app.trackedRequestId()).toBe("r-1");
    let update: Listener | undefined;
    const stop = startClientObservation(
      app,
      vi.fn(async (listener) => {
        update = listener as Listener;
        return () => {};
      }),
    );
    try {
      await vi.advanceTimersByTimeAsync(120_000);
      expect(app.stores.session.getState().load.phase).toBe("pending");
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
      void update?.({ reason: "store" });
      await vi.advanceTimersByTimeAsync(0);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(2);
    } finally {
      stop();
    }
  });
});

describe("a session read through the node's operator access", () => {
  it("is read at the session read gap, having no replica here to raise events", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    const api = bridgeApi();
    const session = {
      ...sessionSnapshot({
        sessionId: "s-1",
        nodeDid: NODE_DID,
        turnState: "running",
        latestRequestId: "r-1",
        liveCursor: null,
        timelineItems: [],
      }),
      readThroughOperator: true,
    };
    api.fetchSessionSnapshot.mockResolvedValue(session);
    const app = testApp({ api, snapshot: running, session });
    expect(app.trackedRequestId()).toBe("r-1");
    const stop = startClientObservation(
      app,
      vi.fn(async () => () => {}),
    );
    try {
      await vi.advanceTimersByTimeAsync(0);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(1_500);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(1_500);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(3);
      stop();
      await vi.advanceTimersByTimeAsync(10_000);
      expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(3);
    } finally {
      stop();
    }
  });
});

describe("Try again on a session that couldn't be read", () => {
  it("reads the session again, not only the node list", async () => {
    const api = {
      fetchDesktopSnapshot: vi.fn(async () => running),
      fetchSessionSnapshot: vi.fn().mockRejectedValueOnce(new Error("database busy")),
    };
    const app = testApp({
      api,
      snapshot: running,
      selection: { nodeDid: NODE_DID, sessionId: "s-1" },
    });
    await act(() => app.actions.refreshSession("s-1"));
    renderIn(app, <LoadingStatus />);
    expect(screen.getByText("Couldn’t load this session")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(2));
    expect(api.fetchDesktopSnapshot).toHaveBeenCalled();
  });
});
