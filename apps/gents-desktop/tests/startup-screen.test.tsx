import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";

import App from "../src/App";
import { StartupScreen } from "../src/components/StartupScreen";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

const savedPeer = {
  peerId: deployment.peerId,
  label: deployment.label,
  agentDid: deployment.agentDid,
  addr: deployment.addr,
  source: deployment.source,
  graphql: deployment.graphql,
};

function snapshot(configured: boolean, running: boolean): DesktopClientSnapshot {
  return {
    bootstrap: {
      ...bootstrap,
      savedPeers: configured ? [savedPeer] : [],
    },
    client: running
      ? {
          localPeerId: "local-peer",
          listenAddresses: ["127.0.0.1:9191"],
          p2pHealth: {
            status: "healthy",
            connectedPeerCount: 1,
            replicatorCount: 1,
            consecutiveFailures: 0,
          },
          bootstrapErrors: [],
          lastMutationError: null,
          focusedRequestId: null,
          configuredPeerCount: 1,
          dialedPeerCount: 1,
          peerIssueCount: 0,
          rowCount: 42,
          approxSerializedBytes: 2048,
          deployments: [deployment],
        }
      : null,
  };
}

function bridge(
  fetchDesktopSnapshot: DesktopApiAdapter["fetchDesktopSnapshot"],
  startDesktopClient: DesktopApiAdapter["startDesktopClient"],
) {
  const api = {
    fetchDesktopSnapshot,
    fetchSessionSnapshot: vi.fn(async () => null),
    setSelectedAgent: vi.fn(async () => undefined),
    startDesktopClient,
  } as unknown as DesktopApiAdapter;
  const listenToUpdates: DesktopClientUpdatedListenerFactory = async () => () => {};
  return { api, listenToUpdates };
}

describe("desktop startup screen", () => {
  it("names hosted-agent restoration instead of misreporting a configuration read", async () => {
    const status = deferred<{
      state: "disabled";
      autoStart: false;
      agentName: null;
      agentDid: null;
      graphql: null;
      error: null;
    }>();
    const base = bridge(
      vi.fn(async () => snapshot(false, false)),
      vi.fn(async () => snapshot(false, true)),
    );
    render(
      <App
        bridge={{
          ...base,
          supportsManagedServer: true,
          api: {
            ...base.api,
            managedServerStatus: vi.fn(() => status.promise),
            startManagedServer: vi.fn(),
          },
        }}
      />,
    );

    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Checking the hosted agent",
    );
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Restore hosted agentWorking",
    );
    expect(base.api.fetchDesktopSnapshot).not.toHaveBeenCalled();

    status.resolve({
      state: "disabled",
      autoStart: false,
      agentName: null,
      agentDid: null,
      graphql: null,
      error: null,
    });
    await waitFor(() => {
      expect(screen.getByTestId("fleet-empty")).toBeInTheDocument();
    });
  });

  it("keeps hosted-agent restoration failure in the startup retry flow", async () => {
    const base = bridge(
      vi.fn(async () => snapshot(false, false)),
      vi.fn(async () => snapshot(false, true)),
    );
    const managedServerStatus = vi
      .fn()
      .mockRejectedValueOnce(new Error("hosted agent unavailable"))
      .mockResolvedValueOnce({
        state: "disabled",
        autoStart: false,
        agentName: null,
        agentDid: null,
        graphql: null,
        error: null,
      });
    render(
      <App
        bridge={{
          ...base,
          supportsManagedServer: true,
          api: {
            ...base.api,
            managedServerStatus,
            startManagedServer: vi.fn(),
          },
        }}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("startup-retry")).toBeInTheDocument();
    });
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "hosted agent unavailable",
    );
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "The hosted agent could not be restored",
    );

    await userEvent.click(screen.getByTestId("startup-retry"));
    await waitFor(() => {
      expect(screen.getByTestId("fleet-empty")).toBeInTheDocument();
    });
    expect(managedServerStatus).toHaveBeenCalledTimes(2);
  });

  it("does not invent a synchronization phase while client startup is pending", () => {
    vi.useFakeTimers();
    const { unmount } = render(
      <StartupScreen
        error={null}
        onRetry={vi.fn(async () => undefined)}
        phase="starting-client"
      />,
    );

    act(() => vi.advanceTimersByTime(5_000));

    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Starting the secure client",
    );
    expect(screen.getByTestId("startup-screen")).not.toHaveTextContent(
      "Synchronize agent state",
    );
    unmount();
    vi.useRealTimers();
  });

  it("never flashes enrollment while saved connections load and start", async () => {
    const initial = deferred<DesktopClientSnapshot>();
    const started = deferred<DesktopClientSnapshot>();
    const startDesktopClient = vi.fn(() => started.promise);
    render(
      <App
        bridge={bridge(
          vi.fn(() => initial.promise),
          startDesktopClient,
        )}
      />,
    );

    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Reading saved connections",
    );
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Catalyzing dilithium converters.",
    );
    expect(screen.queryByTestId("fleet-empty")).not.toBeInTheDocument();

    initial.resolve(snapshot(true, false));
    await waitFor(() => expect(startDesktopClient).toHaveBeenCalledTimes(1));
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Starting the secure client",
    );
    expect(screen.queryByTestId("fleet-empty")).not.toBeInTheDocument();

    started.resolve(snapshot(true, true));
    await waitFor(() => {
      expect(screen.getByTestId("fleet-dashboard")).toBeInTheDocument();
    });
    expect(screen.queryByTestId("startup-screen")).not.toBeInTheDocument();
  });

  it("shows enrollment only after configuration is confirmed empty", async () => {
    const initial = deferred<DesktopClientSnapshot>();
    const startDesktopClient = vi.fn(async () => snapshot(false, true));
    render(
      <App
        bridge={bridge(
          vi.fn(() => initial.promise),
          startDesktopClient,
        )}
      />,
    );

    expect(screen.getByTestId("startup-screen")).toBeInTheDocument();
    expect(screen.queryByTestId("fleet-empty")).not.toBeInTheDocument();

    initial.resolve(snapshot(false, false));
    await waitFor(() => {
      expect(screen.getByTestId("fleet-empty")).toBeInTheDocument();
    });
    expect(startDesktopClient).not.toHaveBeenCalled();
  });

  it("offers a retry when reading saved configuration fails", async () => {
    const fetchDesktopSnapshot = vi
      .fn<DesktopApiAdapter["fetchDesktopSnapshot"]>()
      .mockRejectedValueOnce(new Error("configuration unavailable"))
      .mockResolvedValueOnce(snapshot(false, false));
    render(
      <App
        bridge={bridge(
          fetchDesktopSnapshot,
          vi.fn(async () => snapshot(false, true)),
        )}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("startup-retry")).toBeInTheDocument();
    });
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "configuration unavailable",
    );
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Saved connections could not be read",
    );

    await userEvent.click(screen.getByTestId("startup-retry"));
    await waitFor(() => {
      expect(screen.getByTestId("fleet-empty")).toBeInTheDocument();
    });
    expect(fetchDesktopSnapshot).toHaveBeenCalledTimes(2);
  });
});
