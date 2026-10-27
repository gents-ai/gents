import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  BridgeInvokeError,
  type DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";

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
  let update: ((event: { reason?: string }) => Promise<void>) | undefined;
  const stop = startClientObservation(
    app,
    vi.fn(async (listener) => {
      update = listener as typeof update;
      return () => {};
    }),
  );
  /* to the nearest 100 ms: each read settles a tick after its timer */
  const gaps = () =>
    started.slice(1).map((at, i) => Math.round((at - started[i]) / 100) * 100);
  const sendUpdate = async () => {
    void update?.({ reason: "store" });
    await vi.advanceTimersByTimeAsync(0);
  };
  return { app, api, stop, gaps, started, sendUpdate };
}

afterEach(() => vi.useRealTimers());

describe("reading a session with an open request", () => {
  it("keeps pace for a minute without change, then slows, and keeps pace again once it changes", async () => {
    let changing = false;
    const run = observe({ readMs: 0, changing: () => changing });
    try {
      await vi.advanceTimersByTimeAsync(77_000);
      expect(run.gaps()).toEqual([...Array(40).fill(1_500), 8_000, 8_000]);
      changing = true;
      await vi.advanceTimersByTimeAsync(8_000 + 1_500 + 1_500);
      expect(run.gaps().slice(42)).toEqual([8_000, 1_500, 1_500]);
    } finally {
      run.stop();
    }
  });

  it("waits at least as long as the last read took", async () => {
    const run = observe({ readMs: 4_000, changing: () => true });
    try {
      await vi.advanceTimersByTimeAsync(30_000);
      expect(new Set(run.gaps())).toEqual(new Set([8_000]));
    } finally {
      run.stop();
    }
  });

  it("reads a changing session on an update, but never back to back", async () => {
    const run = observe({ readMs: 1_000, changing: () => true });
    try {
      await vi.advanceTimersByTimeAsync(1_000);
      expect(run.started).toHaveLength(1);
      await run.sendUpdate();
      await vi.advanceTimersByTimeAsync(500);
      await run.sendUpdate();
      expect(run.started).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(600);
      await run.sendUpdate();
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

  it("leaves updates to the slower pace once the session has gone quiet", async () => {
    const run = observe({ readMs: 0, changing: () => false });
    try {
      await vi.advanceTimersByTimeAsync(61_000);
      const before = run.started.length;
      for (let i = 0; i < 20; i++) {
        await run.sendUpdate();
        await vi.advanceTimersByTimeAsync(250);
      }
      expect(run.started).toHaveLength(before);
      await vi.advanceTimersByTimeAsync(3_000);
      expect(run.started).toHaveLength(before + 1);
    } finally {
      run.stop();
    }
  });

  it("slows down once the session's request has been missing for a minute", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    const api = bridgeApi();
    const started: number[] = [];
    api.fetchSessionSnapshot.mockImplementation(async () => {
      started.push(performance.now());
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
    const stop = startClientObservation(
      app,
      vi.fn(async () => () => {}),
    );
    try {
      await vi.advanceTimersByTimeAsync(62_000);
      const quietFrom = started.length;
      await vi.advanceTimersByTimeAsync(60_000);
      expect(app.stores.session.getState().load.phase).toBe("pending");
      expect(started.length - quietFrom).toBeLessThanOrEqual(8);
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
