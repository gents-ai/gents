import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopClientSnapshot,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";

import { useDesktopRuntime } from "../src/hooks/useDesktopRuntime";
import { node, testApp } from "./app-fixture";

const AGENT = "did:key:here";
const running = {
  bootstrap: { clientStateExists: true, savedPeers: [] },
  client: {
    deployments: [
      node({
        agentDid: AGENT,
        sessions: [
          { sessionId: "s-1", agentDid: AGENT, requesterDid: null },
          { sessionId: "s-2", agentDid: AGENT, requesterDid: null },
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
  it("keeps one bridge listener while the person moves between sessions", async () => {
    const listen = vi.fn<DesktopClientUpdatedListenerFactory>(async () => () => {});
    const app = testApp({
      api: bridgeApi(),
      snapshot: running,
      selection: { agentDid: AGENT, sessionId: "s-1" },
    });
    renderHook(() => useDesktopRuntime(app, listen));
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
            node({ agentDid: "did:key:there" }),
          ],
        },
      },
      selection: { agentDid: AGENT, sessionId: "s-1" },
    });
    renderHook(() =>
      useDesktopRuntime(
        app,
        vi.fn(async () => () => {}),
      ),
    );
    await waitFor(() => expect(api.setSelectedAgent).toHaveBeenCalledWith(AGENT));

    act(() => app.actions.selectSession("s-2"));
    await waitFor(() =>
      expect(api.fetchSessionSnapshot).toHaveBeenCalledWith(
        "s-2",
        AGENT,
        null,
        expect.anything(),
      ),
    );

    act(() => app.actions.selectAgent("did:key:there"));
    await waitFor(() =>
      expect(api.setSelectedAgent).toHaveBeenLastCalledWith("did:key:there"),
    );
  });
});
