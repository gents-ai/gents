import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopClientUpdatedListenerFactory,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";

import App from "../src/App";
import { StartupScreen } from "../src/components/StartupScreen";
import { setTimingForTests } from "../src/hooks/timing";
import { renderIn, testApp } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import { deferred } from "./deferred";

const shownStep = (text: string) =>
  waitFor(() => expect(screen.getByTestId("startup-step")).toHaveTextContent(text), {
    timeout: 2_000,
  });

const savedPeer = {
  label: deployment.label,
  nodeDid: deployment.nodeDid,

  graphql: deployment.graphql,
  local: false,
};

function snapshot(configured: boolean, running: boolean): DesktopClientSnapshot {
  return {
    bootstrap: {
      ...bootstrap,
      initConfigStatus: configured ? "configured" : "absent",
      ...(configured
        ? {}
        : { initNodeName: null, initNodeDid: null, nodeHomeExists: false }),
      savedRuntimes: configured ? [savedPeer] : [],
    },
    client: running
      ? {
          clientDid: "did:key:startup-fixture",

          syncHealth: {
            state: "healthy",
            cause: null,
            nextAction: null,

            runtimeCount: 1,
            connectedRuntimeCount: 1,
          },
          accessRequests: [],
          rowCount: 42,
          approxSerializedBytes: 2048,
          operatorConfigAuthoritativeNodeDids: configured ? [deployment.nodeDid] : [],
          deployments: [deployment],
          unkeptChanges: [],
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
    setSelectedNode: vi.fn(async () => undefined),
    startDesktopClient,
    getInferenceSetupCatalog: vi.fn(async () => ({
      contractVersion: 1,
      defaultsVersion: "test",
      providers: [
        {
          id: "local",
          displayName: "Local",
          description: "OpenAI-compatible local server.",
          authMethods: ["optional_api_key"],
          authOptions: [
            {
              method: "optional_api_key",
              displayName: "Endpoint + optional key",
              defaultEndpoint: "http://127.0.0.1:11434/v1",
            },
          ],
          defaultAuthMethod: "optional_api_key",
          defaultEndpoint: "http://127.0.0.1:11434/v1",
        },
      ],
    })),
  } as unknown as DesktopApiAdapter;
  const listenToUpdates: DesktopClientUpdatedListenerFactory = async () => () => {};
  return { api, listenToUpdates };
}

describe("desktop startup screen", () => {
  it("explains an unreadable saved node config and offers a retry", () => {
    const refreshed = vi.fn(async () => snapshot(true, true));
    const app = testApp({ api: { fetchDesktopSnapshot: refreshed } });
    renderIn(app, <StartupScreen gate="unreadable" />);

    expect(screen.getByTestId("startup-configuration-pending")).toHaveTextContent(
      "Unable to read saved node settings",
    );
    expect(screen.getByTestId("startup-configuration-pending")).toHaveTextContent(
      "The saved node settings could not be read. Try again to reload them.",
    );
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  });

  it("offers to connect a desktop the terminal-made node does not know", async () => {
    const served = {
      state: "running" as const,
      autoStart: true,
      nodeName: "Forge",
      nodeDid: deployment.nodeDid,
      graphql: deployment.graphql,
      effectiveToolCeiling: null,
      effectiveToolRoot: null,
      suggestedToolRoot: "/Users/test",
      pairingReady: false,
      approvalRequired: false,
      runtimeBooting: false,
      error: null,
    };
    let nodeState: "stopped" | "running" = "stopped";
    const enrollManagedServer = vi
      .fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error("CLI client identity is unavailable"))
      .mockImplementation(async () => {});
    const fetchDesktopSnapshot = vi.fn(async () => snapshot(true, true));
    const terminalMade = testApp({
      api: {
        managedServerStatus: vi.fn(async () => ({ ...served, state: nodeState })),
        enrollManagedServer,
        fetchDesktopSnapshot,
      },
    });
    const pending = renderIn(terminalMade, <StartupScreen gate="not_connected" />);
    const shown = within(pending.container);

    expect(shown.getByTestId("startup-configuration-pending")).toHaveTextContent(
      "This desktop is not connected to your node",
    );
    expect(await shown.findByTestId("startup-connect-blocked")).toHaveTextContent(
      "The node on this computer is not running. Start it in a terminal with `gents runtime server`, then choose Try again.",
    );

    nodeState = "running";
    await userEvent.click(shown.getByTestId("startup-connect-retry"));
    await waitFor(() =>
      expect(shown.queryByTestId("startup-connect-blocked")).not.toBeInTheDocument(),
    );
    const connect = shown.getByTestId("startup-connect-desktop");
    const reads = fetchDesktopSnapshot.mock.calls.length;

    await userEvent.click(connect);
    expect(await shown.findByTestId("startup-connect-error")).toHaveTextContent(
      "Couldn’t connect this desktop: CLI client identity is unavailable",
    );
    expect(fetchDesktopSnapshot).toHaveBeenCalledTimes(reads);

    await userEvent.click(connect);
    await waitFor(() => expect(fetchDesktopSnapshot).toHaveBeenCalledTimes(reads + 1));
    expect(enrollManagedServer).toHaveBeenCalledTimes(2);
  });

  it("names local-agent observation instead of misreporting a configuration read", async () => {
    const status = deferred<ManagedServerStatus>();
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

    expect(screen.getByTestId("startup-step")).toHaveTextContent(
      "Starting your local node",
    );
    expect(screen.getByText("Starting your local node")).toHaveAttribute(
      "data-state",
      "active",
    );
    expect(base.api.fetchDesktopSnapshot).not.toHaveBeenCalled();

    status.resolve({
      state: "disabled",
      autoStart: false,
      nodeName: null,
      nodeDid: null,
      graphql: null,
      effectiveToolCeiling: null,
      effectiveToolRoot: null,
      suggestedToolRoot: "/Users/test",
      pairingReady: false,
      approvalRequired: false,
      runtimeBooting: false,
      error: null,
    });
    await waitFor(() => {
      expect(screen.getByTestId("setup-screen")).toBeInTheDocument();
    });
  });

  it("does not block first-run setup when background-agent observation fails", async () => {
    const base = bridge(
      vi.fn(async () => snapshot(false, false)),
      vi.fn(async () => snapshot(false, true)),
    );
    const managedServerStatus = vi
      .fn()
      .mockRejectedValueOnce(new Error("background node unavailable"));
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
      expect(screen.getByTestId("setup-screen")).toBeInTheDocument();
    });
    expect(screen.queryByTestId("startup-retry")).not.toBeInTheDocument();

    await waitFor(() => {
      expect(managedServerStatus).toHaveBeenCalledTimes(3);
    });
  });

  it("does not invent a synchronization phase while client startup is pending", () => {
    vi.useFakeTimers();
    const app = testApp();
    app.stores.client.setState({ startupPhase: "starting-client" });
    const { unmount } = renderIn(app, <StartupScreen />);

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
    const fetchDesktopSnapshot = vi
      .fn()
      .mockReturnValueOnce(initial.promise)
      .mockReturnValue(started.promise);
    render(<App bridge={bridge(fetchDesktopSnapshot, startDesktopClient)} />);

    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Reading saved connections",
    );
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Catalyzing dilithium converters.",
    );
    expect(screen.queryByTestId("setup-screen")).not.toBeInTheDocument();

    initial.resolve(snapshot(true, false));
    await waitFor(() => expect(startDesktopClient).toHaveBeenCalledTimes(1));
    await shownStep("Starting the secure client");
    expect(screen.queryByTestId("setup-screen")).not.toBeInTheDocument();

    started.resolve(snapshot(true, true));
    await waitFor(() => {
      expect(screen.getByTestId("setup-screen")).toBeInTheDocument();
    });
    expect(
      screen.getByRole("heading", { name: "Choose an inference provider" }),
    ).toBeInTheDocument();
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
    expect(screen.queryByTestId("setup-screen")).not.toBeInTheDocument();

    initial.resolve(snapshot(false, false));
    await waitFor(() => {
      expect(screen.getByTestId("setup-screen")).toBeInTheDocument();
    });
    expect(startDesktopClient).not.toHaveBeenCalled();
  });

  it("offers the local node instead of waiting on a client autostart declined", async () => {
    const stopped = {
      state: "stopped" as const,
      autoStart: false,
      nodeName: "Forge",
      nodeDid: null,
      graphql: null,
      effectiveToolCeiling: null,
      effectiveToolRoot: null,
      suggestedToolRoot: "/Users/test",
      pairingReady: false,
      approvalRequired: false,
      runtimeBooting: false,
      error: null,
    };
    const localOnly: DesktopClientSnapshot = {
      bootstrap: {
        ...bootstrap,
        initConfigStatus: "absent",
        initNodeName: null,
        initNodeDid: null,
        nodeHomeExists: false,
        clientStateExists: false,
        savedRuntimes: [{ ...savedPeer, local: true }],
      },
      client: null,
    };
    const startDesktopClient = vi.fn(async () => snapshot(true, true));
    const base = bridge(
      vi.fn(async () => localOnly),
      startDesktopClient,
    );
    render(
      <App
        bridge={{
          ...base,
          supportsManagedServer: true,
          api: {
            ...base.api,
            managedServerStatus: vi.fn(async () => stopped),
            startManagedServer: vi.fn(),
          },
        }}
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("setup-screen")).toBeInTheDocument();
    });
    expect(screen.queryByTestId("startup-screen")).not.toBeInTheDocument();
    expect(screen.queryByText(/Starting the secure client/)).not.toBeInTheDocument();
    expect(screen.getByText("Local node")).toBeInTheDocument();
    expect(screen.getByTestId("setup-next")).toBeInTheDocument();
    expect(startDesktopClient).not.toHaveBeenCalled();
  });

  it("starts the client for a local peer whose runtime is serving", async () => {
    const running = {
      state: "running" as const,
      autoStart: true,
      nodeName: "Forge",
      nodeDid: null,
      graphql: null,
      effectiveToolCeiling: null,
      effectiveToolRoot: null,
      suggestedToolRoot: "/Users/test",
      pairingReady: true,
      approvalRequired: false,
      runtimeBooting: false,
      error: null,
    };
    const started = deferred<DesktopClientSnapshot>();
    const localOnly: DesktopClientSnapshot = {
      bootstrap: {
        ...bootstrap,
        initConfigStatus: "absent",
        initNodeName: null,
        initNodeDid: null,
        nodeHomeExists: false,
        clientStateExists: false,
        savedRuntimes: [{ ...savedPeer, local: true }],
      },
      client: null,
    };
    const startDesktopClient = vi.fn(() => started.promise);
    const base = bridge(
      vi
        .fn()
        .mockResolvedValueOnce(localOnly)
        .mockImplementation(() => started.promise),
      startDesktopClient,
    );
    render(
      <App
        bridge={{
          ...base,
          supportsManagedServer: true,
          api: {
            ...base.api,
            managedServerStatus: vi.fn(async () => running),
            startManagedServer: vi.fn(),
          },
        }}
      />,
    );

    await waitFor(() => expect(startDesktopClient).toHaveBeenCalledTimes(1));
    await shownStep("Starting the secure client");
    started.resolve(snapshot(true, true));
    await waitFor(() => {
      expect(screen.queryByTestId("startup-screen")).not.toBeInTheDocument();
    });
  });

  it("starts the client again when a retry follows a failed start", async () => {
    let running = false;
    const fetchDesktopSnapshot = vi.fn(async () => snapshot(true, running));
    const startDesktopClient = vi
      .fn<DesktopApiAdapter["startDesktopClient"]>()
      .mockRejectedValueOnce(new Error("client failed to start"))
      .mockImplementation(async () => {
        running = true;
        return snapshot(true, true);
      });
    render(<App bridge={bridge(fetchDesktopSnapshot, startDesktopClient)} />);

    await userEvent.click(await screen.findByTestId("startup-retry"));

    await waitFor(() => expect(startDesktopClient).toHaveBeenCalledTimes(2));
    await waitFor(() => {
      expect(screen.queryByTestId("startup-screen")).not.toBeInTheDocument();
    });
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
      expect(screen.getByTestId("setup-screen")).toBeInTheDocument();
    });
    expect(fetchDesktopSnapshot).toHaveBeenCalledTimes(2);
  });
});

describe("startup waits on this device's saved settings, never on a node", () => {
  afterEach(() => setTimingForTests(null));

  const runningNode = (pairingReady: boolean): ManagedServerStatus => ({
    state: "running",
    autoStart: true,
    nodeName: "Forge",
    nodeDid: deployment.nodeDid,
    graphql: deployment.graphql,
    effectiveToolCeiling: null,
    effectiveToolRoot: null,
    suggestedToolRoot: "/Users/test",
    pairingReady,
    approvalRequired: false,
    runtimeBooting: false,
    error: null,
  });

  /* this desktop's local node: init.json is read, but the node's own
     configuration is not, since the node has not answered yet */
  function unansweredLocal(): DesktopClientSnapshot {
    const running = snapshot(true, true);
    return {
      bootstrap: { ...running.bootstrap, initNodeDid: deployment.nodeDid },
      client: {
        ...running.client!,
        operatorConfigAuthoritativeNodeDids: [],
        deployments: [{ ...deployment, runtimeConnection: "connecting" }],
      },
    };
  }

  function withLocalNode(
    fetchDesktopSnapshot: DesktopApiAdapter["fetchDesktopSnapshot"],
    managedServerStatus: () => Promise<ManagedServerStatus>,
  ) {
    const base = bridge(
      fetchDesktopSnapshot,
      vi.fn(async () => unansweredLocal()),
    );
    return {
      ...base,
      supportsManagedServer: true,
      api: { ...base.api, managedServerStatus, startManagedServer: vi.fn() },
    };
  }

  it("shows the shell once saved settings are read, with the node still connecting", async () => {
    render(
      <App
        bridge={withLocalNode(
          vi.fn(async () => unansweredLocal()),
          vi.fn(async () => runningNode(true)),
        )}
      />,
    );

    expect(await screen.findByTestId("app-shell")).toBeInTheDocument();
    expect(screen.queryByText("Loading saved node settings")).not.toBeInTheDocument();
    expect(
      screen.queryByTestId("startup-configuration-pending"),
    ).not.toBeInTheDocument();
    expect(screen.queryByTestId("startup-screen")).not.toBeInTheDocument();
    expect(screen.getAllByText(/Connecting/).length).toBeGreaterThan(0);
  });

  it("shows the shell when the local node's status cannot be read", async () => {
    render(
      <App
        bridge={withLocalNode(
          vi.fn(async () => unansweredLocal()),
          vi.fn(async () => {
            throw new Error("status unavailable");
          }),
        )}
      />,
    );

    expect(await screen.findByTestId("app-shell")).toBeInTheDocument();
    expect(
      screen.queryByTestId("startup-configuration-pending"),
    ).not.toBeInTheDocument();
  });

  it("shows the shell for a saved remote node that is still connecting", async () => {
    const remote: DesktopClientSnapshot = {
      bootstrap: {
        ...bootstrap,
        initConfigStatus: "absent",
        initNodeName: null,
        initNodeDid: null,
        nodeHomeExists: false,
        savedRuntimes: [savedPeer],
      },
      client: {
        ...snapshot(false, true).client!,
        deployments: [{ ...deployment, runtimeConnection: "connecting" }],
      },
    };
    render(
      <App
        bridge={bridge(
          vi.fn(async () => remote),
          vi.fn(async () => remote),
        )}
      />,
    );

    expect(await screen.findByTestId("app-shell")).toBeInTheDocument();
    expect(
      screen.queryByTestId("startup-configuration-pending"),
    ).not.toBeInTheDocument();
    expect(screen.getAllByText(/Connecting/).length).toBeGreaterThan(0);
  });

  it("names a settings read that never answers and retries it", async () => {
    setTimingForTests({ configurationReadTimeoutMs: 50 });
    const fetchDesktopSnapshot = vi
      .fn<DesktopApiAdapter["fetchDesktopSnapshot"]>()
      .mockReturnValueOnce(new Promise(() => {}))
      .mockResolvedValue(snapshot(false, false));
    render(
      <App
        bridge={bridge(
          fetchDesktopSnapshot,
          vi.fn(async () => snapshot(false, true)),
        )}
      />,
    );

    expect(await screen.findByTestId("startup-retry")).toBeInTheDocument();
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "Saved connections could not be read",
    );
    expect(screen.getByTestId("startup-screen")).toHaveTextContent(
      "did not finish within",
    );

    await userEvent.click(screen.getByTestId("startup-retry"));
    expect(await screen.findByTestId("setup-screen")).toBeInTheDocument();
    expect(fetchDesktopSnapshot).toHaveBeenCalledTimes(2);
  });

  it("leaves the error once a settings read answers after startup gave up on it", async () => {
    setTimingForTests({ configurationReadTimeoutMs: 50 });
    const late = deferred<DesktopClientSnapshot>();
    const fetchDesktopSnapshot = vi.fn().mockImplementationOnce(() => late.promise);
    render(
      <App
        bridge={bridge(
          fetchDesktopSnapshot,
          vi.fn(async () => snapshot(false, true)),
        )}
      />,
    );

    expect(await screen.findByTestId("startup-retry")).toBeInTheDocument();
    await act(async () => late.resolve(snapshot(false, false)));
    expect(await screen.findByTestId("setup-screen")).toBeInTheDocument();
    expect(screen.queryByTestId("startup-retry")).not.toBeInTheDocument();
    expect(fetchDesktopSnapshot).toHaveBeenCalledTimes(1);
  });

  it("ignores a settings read a retry superseded", async () => {
    setTimingForTests({ configurationReadTimeoutMs: 50 });
    const superseded = deferred<DesktopClientSnapshot>();
    const retried = deferred<DesktopClientSnapshot>();
    const fetchDesktopSnapshot = vi
      .fn()
      .mockImplementationOnce(() => superseded.promise)
      .mockImplementationOnce(() => retried.promise);
    render(
      <App
        bridge={bridge(
          fetchDesktopSnapshot,
          vi.fn(async () => snapshot(false, true)),
        )}
      />,
    );

    await userEvent.click(await screen.findByTestId("startup-retry"));
    await waitFor(() => expect(fetchDesktopSnapshot).toHaveBeenCalledTimes(2));
    await act(async () => superseded.resolve(snapshot(false, false)));
    expect(screen.queryByTestId("setup-screen")).not.toBeInTheDocument();

    await act(async () => retried.resolve(snapshot(false, false)));
    expect(await screen.findByTestId("setup-screen")).toBeInTheDocument();
  });
});
