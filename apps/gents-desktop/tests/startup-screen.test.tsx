import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";

import App from "../src/App";
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
  it("never flashes pairing while saved connections load and start", async () => {
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

  it("shows pairing only after configuration is confirmed empty", async () => {
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

    await userEvent.click(screen.getByTestId("startup-retry"));
    await waitFor(() => {
      expect(screen.getByTestId("fleet-empty")).toBeInTheDocument();
    });
    expect(fetchDesktopSnapshot).toHaveBeenCalledTimes(2);
  });
});
