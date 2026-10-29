import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

import { useFirstRun } from "../src/ui/app/useFirstRun";
import { supportsLocalManagedServer } from "../src/lib/shellPlatform";
import { node, publishSnapshot, testApp, withApp } from "./app-fixture";
import { bootstrap } from "./config-panel-wiring/fixtures";

vi.mock("../src/lib/shellPlatform", () => ({
  supportsLocalManagedServer: vi.fn(() => false),
}));

function remoteSnapshot(chatSafe: boolean): DesktopClientSnapshot {
  return {
    bootstrap: {
      ...bootstrap,
      initConfigStatus: "absent",
      initNodeDid: null,
      savedPeers: [
        {
          peerId: "peer-remote",
          label: "Remote",
          nodeDid: "did:key:z6MkRemote",
          addr: "/ip4/203.0.113.7/tcp/9000",
          source: "enrollment",
          graphql: null,
        },
      ],
    },
    client: {
      deployments: [
        node({
          source: "enrollment",
          chatSafe,
          dialSucceeded: false,
          graphql: null,
          nodeConfig: null,
          agentConfigs: [],
          inferenceProfiles: [],
          inferenceBackends: [],
        }),
      ],
      enrollmentRequests: [],
      operatorConfigAuthoritativeNodeDids: [],
    },
  } as unknown as DesktopClientSnapshot;
}

describe("first-run mobile reentry", () => {
  beforeEach(() => vi.mocked(supportsLocalManagedServer).mockReturnValue(false));

  it("reopens an enrolled remote without local init or operator access, while the signed route reconnects offline", () => {
    const app = testApp({ snapshot: remoteSnapshot(false) });
    const first = renderHook(() => useFirstRun(1, true), { wrapper: withApp(app) });
    expect(first.result.current.phase).toBe("done");
    first.unmount();
    const reopened = renderHook(() => useFirstRun(1, true), { wrapper: withApp(app) });
    expect(reopened.result.current.phase).toBe("done");
  });

  it("holds pending enrollment in setup until explicitly completed", () => {
    const pending = remoteSnapshot(false);
    pending.bootstrap.savedPeers = [];
    pending.client!.deployments = [];
    const app = testApp({ snapshot: pending });
    const { result } = renderHook(() => useFirstRun(1, true), {
      wrapper: withApp(app),
    });
    expect(result.current.phase).toBe("active");
    act(() => publishSnapshot(app, remoteSnapshot(true)));
    expect(result.current.phase).toBe("active");
    act(() => result.current.finish());
    expect(result.current.phase).toBe("done");
  });

  it("waits for startup and unreadable bootstrap before deciding", () => {
    const snapshot = remoteSnapshot(true);
    snapshot.bootstrap.initConfigStatus = "unreadable";
    const app = testApp({ snapshot });
    const { result, rerender } = renderHook(({ ready }) => useFirstRun(1, ready), {
      initialProps: { ready: false },
      wrapper: withApp(app),
    });
    expect(result.current.phase).toBe("unknown");
    rerender({ ready: true });
    expect(result.current.phase).toBe("unknown");
    act(() => publishSnapshot(app, remoteSnapshot(true)));
    expect(result.current.phase).toBe("done");
  });

  it("reopens a desktop's saved enrollment without a local home before its route is ready, including after restart", () => {
    vi.mocked(supportsLocalManagedServer).mockReturnValue(true);
    const offline = remoteSnapshot(false);
    offline.client = null;
    const app = testApp({ snapshot: offline });
    const { result, rerender } = renderHook(
      ({ generation, ready }) => useFirstRun(generation, ready),
      { initialProps: { generation: 1, ready: false }, wrapper: withApp(app) },
    );
    expect(result.current.phase).toBe("unknown");
    rerender({ generation: 1, ready: true });
    expect(result.current.phase).toBe("done");
    rerender({ generation: 2, ready: true });
    expect(result.current.phase).toBe("done");
  });

  it("keeps absent local-home setup on a desktop without a saved enrollment", () => {
    vi.mocked(supportsLocalManagedServer).mockReturnValue(true);
    const snapshot = remoteSnapshot(true);
    snapshot.bootstrap.savedPeers = [];
    const app = testApp({ snapshot });
    const { result } = renderHook(() => useFirstRun(1, true), {
      wrapper: withApp(app),
    });
    expect(result.current.phase).toBe("active");
  });
});
