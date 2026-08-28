import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { FleetHostDashboard } from "../src/components/fleet/FleetHostDashboard";
import type {
  BearerPairingResponse,
  BootstrapSummary,
  DeploymentView,
} from "@source-inc/gents-desktop-client";
import { deployment } from "./config-panel-wiring/fixtures";

const inferenceProps = {
  onSaveBackendConfig: vi.fn(async () => undefined),
  onSaveBehaviorConfig: vi.fn(async () => undefined),
  onProbeInferenceEndpoint: vi.fn(async () => ({ reachable: false, models: [] })),
  onCodexLogin: vi.fn(),
  onCancelCodexLogin: vi.fn(async () => undefined),
  onGrokLogin: vi.fn(),
  onCancelGrokLogin: vi.fn(async () => undefined),
};

const bootstrap: BootstrapSummary = {
  defaultAgentHome: "/Users/test/.gents",
  initAgentName: "local-agent",
  initAgentDid: "did:key:z6MkLocal",
  initToolCeiling: "read-write",
  initToolRoot: "/Users/test/project",
  desktopHome: "/Users/test/Library/Application Support/gents-desktop",
  peerDirectoryPath: "/Users/test/Library/Application Support/gents-desktop/peers.json",
  nodeDataDir: "/Users/test/Library/Application Support/gents-desktop/node",
  logFilePath: "/Users/test/Library/Application Support/gents-desktop/desktop.log",
  agentHomeExists: true,
  desktopHomeExists: true,
  peerDirectoryExists: false,
  savedPeers: [],
};

describe("FleetHostDashboard add connection flow", () => {
  it("connects the local runtime from the fleet empty state", async () => {
    const onInitLocalRuntime = vi.fn(async () => undefined);
    const onStartManagedServer = vi.fn(async () => undefined);
    const onCommitManagedServerAutoStart = vi.fn(async () => undefined);

    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={bootstrap}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={vi.fn()}
        onPairBearer={vi.fn()}
        onProbePeerAddress={vi.fn()}
        onInitLocalRuntime={onInitLocalRuntime}
        onStartManagedServer={onStartManagedServer}
        onCommitManagedServerAutoStart={onCommitManagedServerAutoStart}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
        {...inferenceProps}
      />,
    );

    fireEvent.click(screen.getByTestId("fleet-connect-local"));

    await waitFor(() => {
      expect(onStartManagedServer).toHaveBeenCalledOnce();
      expect(onStartManagedServer).toHaveBeenCalledWith("local-agent");
      expect(onCommitManagedServerAutoStart).toHaveBeenCalledWith("local-agent");
      expect(onInitLocalRuntime).toHaveBeenCalledWith("local-agent");
      expect(onStartManagedServer.mock.invocationCallOrder[0]!).toBeLessThan(
        onInitLocalRuntime.mock.invocationCallOrder[0]!,
      );
      expect(onInitLocalRuntime.mock.invocationCallOrder[0]!).toBeLessThan(
        onCommitManagedServerAutoStart.mock.invocationCallOrder[0]!,
      );
    });
  });

  it("discovers peer connection details from a server /status address", async () => {
    const onProbePeerAddress = vi.fn(async () => ({
      agent_name: "worker-a",
      agent_did: "did:key:z6MkWorkerA",
      desktop_graphql: "http://127.0.0.1:9181/api/v0/graphql",
      p2p: {
        p2p_shareable_address: "/ip4/100.73.235.39/tcp/9161/p2p/12D3KooWorker",
      },
    }));
    const onAddPeer = vi.fn(async () => undefined);

    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={onAddPeer}
        onPairBearer={vi.fn()}
        onProbePeerAddress={onProbePeerAddress}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
        {...inferenceProps}
      />,
    );

    fireEvent.change(screen.getByTestId("fleet-add-server-address"), {
      target: { value: "http://127.0.0.1:9181" },
    });
    fireEvent.click(screen.getByTestId("fleet-fetch-status"));

    await waitFor(() => {
      expect(onProbePeerAddress).toHaveBeenCalledWith("http://127.0.0.1:9181");
      expect(onAddPeer).toHaveBeenCalledWith({
        label: "worker-a",
        agentDid: "did:key:z6MkWorkerA",
        addr: "/ip4/100.73.235.39/tcp/9161/p2p/12D3KooWorker",
        graphql: "http://127.0.0.1:9181/api/v0/graphql",
      });
    });
  });

  it("keeps discovered details available when adding the connection fails", async () => {
    const onProbePeerAddress = vi.fn(async () => ({
      agent_name: "api-gateway",
      agent_did: "did:key:z6MkGateway",
      p2p_shareable_address: "iroh://gateway",
    }));

    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={vi.fn(async () => {
          throw new Error("connection rejected");
        })}
        onPairBearer={vi.fn()}
        onProbePeerAddress={onProbePeerAddress}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
        {...inferenceProps}
      />,
    );

    fireEvent.change(screen.getByTestId("fleet-add-server-address"), {
      target: { value: "127.0.0.1:9181" },
    });
    fireEvent.click(screen.getByTestId("fleet-fetch-status"));

    await waitFor(() => {
      expect(screen.getByTestId("fleet-add-label")).toHaveValue("api-gateway");
      expect(screen.getByTestId("fleet-add-agent-did")).toHaveValue(
        "did:key:z6MkGateway",
      );
      expect(screen.getByTestId("fleet-add-addr")).toHaveValue("iroh://gateway");
      expect(screen.getByTestId("fleet-import-status")).toHaveTextContent(
        "Fetched /status",
      );
      expect(
        (screen.getByTestId("fleet-add-connection-json") as HTMLTextAreaElement).value,
      ).toContain('"agent_name": "api-gateway"');
      expect(screen.getByText("connection rejected")).toBeInTheDocument();
    });
  });

  it("surfaces a P2P-disabled discovery result beside the fetch controls", async () => {
    const onProbePeerAddress = vi.fn(async () => ({
      agent_name: "amy",
      agent_did: "did:key:z6MkAmy",
      p2p_transport: "none",
      p2p: {
        enabled: false,
        p2p_listen_addresses: [],
      },
    }));

    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={vi.fn()}
        onPairBearer={vi.fn()}
        onProbePeerAddress={onProbePeerAddress}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
        {...inferenceProps}
      />,
    );

    fireEvent.change(screen.getByTestId("fleet-add-server-address"), {
      target: { value: "http://amy.local:9191" },
    });
    fireEvent.click(screen.getByTestId("fleet-fetch-status"));

    await waitFor(() => {
      expect(screen.getByTestId("fleet-import-status")).toHaveTextContent(
        "This runtime has P2P disabled",
      );
    });
    expect(screen.getByTestId("fleet-add-label")).toHaveValue("");
    expect(screen.getByTestId("fleet-add-server-address")).toHaveValue(
      "http://amy.local:9191",
    );
  });

  it("saves a typed GraphQL endpoint when manually adding a peer", async () => {
    const onProbePeerAddress = vi.fn();
    const onAddPeer = vi.fn(async () => undefined);

    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={onAddPeer}
        onPairBearer={vi.fn()}
        onProbePeerAddress={onProbePeerAddress}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
        {...inferenceProps}
      />,
    );

    fireEvent.change(screen.getByTestId("fleet-add-server-address"), {
      target: { value: "http://100.73.235.38:9181/api/v0/graphql" },
    });
    fireEvent.click(screen.getByText("Enter connection details manually"));
    fireEvent.change(screen.getByTestId("fleet-add-label"), {
      target: { value: "studio-1-steward" },
    });
    fireEvent.change(screen.getByTestId("fleet-add-agent-did"), {
      target: { value: "did:key:z6MkStudio" },
    });
    fireEvent.change(screen.getByTestId("fleet-add-addr"), {
      target: {
        value: "/ip4/100.73.235.38/tcp/9161/p2p/12D3KooStudio",
      },
    });
    fireEvent.click(screen.getByTestId("fleet-add-submit"));

    await waitFor(() => {
      expect(onProbePeerAddress).not.toHaveBeenCalled();
      expect(onAddPeer).toHaveBeenCalledWith({
        label: "studio-1-steward",
        agentDid: "did:key:z6MkStudio",
        addr: "/ip4/100.73.235.38/tcp/9161/p2p/12D3KooStudio",
        graphql: "http://100.73.235.38:9181/api/v0/graphql",
      });
    });
  });

  it("keeps verified pairing readiness visible after closing the add panel", async () => {
    const onPairBearer = vi.fn(async () => ({
      bootstrap: {} as BearerPairingResponse["bootstrap"],
      client: null,
      pairing: {
        peerId: "peer-steward",
        label: "amygdalabook-steward",
        addr: "iroh://steward",
        issuerDid: "did:key:zSteward",
        claimantDid: "did:key:zPhone",
        networkId: "steward-network",
        template: "conversation",
        connected: true,
        claimSubmitted: true,
        endpointPublished: true,
        replicationConfigured: true,
        membershipObserved: true,
        bidirectionalReplicationObserved: true,
      },
    }));

    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={bootstrap}
        deployments={[deployment]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={vi.fn()}
        onPairBearer={onPairBearer}
        onProbePeerAddress={vi.fn()}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Add Agent" }));
    fireEvent.click(screen.getByText("Use a signed pairing invite"));
    fireEvent.change(screen.getByTestId("fleet-pair-label"), {
      target: { value: "amygdalabook-steward" },
    });
    fireEvent.change(screen.getByTestId("fleet-pair-token"), {
      target: { value: "dabear1-signed-invite" },
    });
    fireEvent.click(screen.getByTestId("fleet-pair-submit"));

    await waitFor(() => {
      expect(screen.queryByTestId("fleet-pair-submit")).not.toBeInTheDocument();
      expect(screen.getByTestId("fleet-pair-status")).toHaveTextContent(
        "amygdalabook-steward is ready",
      );
    });
  });
});

describe("FleetHostDashboard fleet-level P2P repair", () => {
  function renderFleet(deployments: DeploymentView[], onRepairP2P = vi.fn()) {
    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={bootstrap}
        deployments={deployments}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={vi.fn()}
        onPairBearer={vi.fn()}
        onProbePeerAddress={vi.fn()}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={onRepairP2P}
        {...inferenceProps}
      />,
    );
    return onRepairP2P;
  }

  it("hides the reconnect control while every connection is healthy", () => {
    renderFleet([{ ...deployment, dialSucceeded: true, lastError: null }]);
    expect(screen.queryByTestId("fleet-repair-p2p")).not.toBeInTheDocument();
  });

  it("shows the reconnect control and fires the repair when a peer is unhealthy", () => {
    const onRepairP2P = renderFleet([{ ...deployment, dialSucceeded: false }]);
    const repair = screen.getByTestId("fleet-repair-p2p");
    fireEvent.click(repair);
    expect(onRepairP2P).toHaveBeenCalledTimes(1);
  });
});

describe("FleetHostDashboard per-deployment inference status", () => {
  function renderFleet(deployments: DeploymentView[]) {
    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={bootstrap}
        deployments={deployments}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={vi.fn()}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
        {...inferenceProps}
      />,
    );
  }

  it("hides setup status when the agent already has a usable backend", () => {
    renderFleet([deployment]);
    expect(
      screen.queryByTestId("fleet-inference-setup-peer-1"),
    ).not.toBeInTheDocument();
  });

  it("places setup status on the affected deployment and opens its wizard", () => {
    renderFleet([{ ...deployment, inferenceBackends: [] }]);
    expect(screen.queryByText("Finish setting up")).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId("fleet-inference-setup-peer-1"));
    expect(screen.getByTestId("inference-wizard")).toBeInTheDocument();
    expect(screen.getByTestId("inference-option-codex")).toBeInTheDocument();
  });

  it("accepts the documented seeded local backend as configured", () => {
    renderFleet([
      {
        ...deployment,
        source: "local-standard",
        inferenceBackends: [
          {
            ...deployment.inferenceBackends[0]!,
            endpoint: "http://127.0.0.1:8080/v1",
            models: ["google/gemma-4-12B-it-qat-q4_0-gguf"],
          },
        ],
      },
    ]);

    expect(screen.queryByTestId("fleet-inference-callout")).not.toBeInTheDocument();
    expect(screen.queryByTestId("inference-wizard")).not.toBeInTheDocument();
  });
});
