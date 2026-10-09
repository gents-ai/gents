import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopClientSnapshot,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";

import { useClientRuntime } from "../src/hooks/useClientRuntime";
import { startClientObservation } from "../src/hooks/clientObservation";
import { node, testApp } from "./app-fixture";
import { liveAssistant, sessionSnapshot } from "./timeline-fixture";

const NODE_DID = "did:key:here";
const running = {
  bootstrap: { clientStateExists: true, savedPeers: [] },
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

/* every bridge call answers; the client is running throughout */
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

describe("following the client while it runs", () => {
  it.each([false, true])(
    "paces polling by live-source availability (%s)",
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
        stop = startClientObservation(
          app,
          vi.fn(async () => () => {}),
        );
        await vi.advanceTimersByTimeAsync(0);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
        const interval = hasLive ? 250 : 1_500;
        await vi.advanceTimersByTimeAsync(interval - 1);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
        expect(api.fetchSessionLiveDelta).not.toHaveBeenCalled();
        await vi.advanceTimersByTimeAsync(1);
        expect(api.fetchSessionLiveDelta).toHaveBeenCalledTimes(hasLive ? 1 : 0);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(hasLive ? 1 : 2);
        stop();
        await vi.advanceTimersByTimeAsync(3_000);
        expect(api.fetchSessionSnapshot).toHaveBeenCalledTimes(hasLive ? 1 : 2);
        expect(api.fetchSessionLiveDelta).toHaveBeenCalledTimes(hasLive ? 1 : 0);
      } finally {
        stop?.();
        vi.useRealTimers();
      }
    },
  );

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
});
