import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { FleetDashboard } from "../src/components/fleet/FleetDashboard";
import type { BootstrapSummary, DeploymentView } from "../src/lib/types";
import { deployment } from "./config-panel-wiring/fixtures";

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

describe("FleetDashboard add connection flow", () => {
  it("connects the local runtime from the fleet empty state", async () => {
    const onInitLocalRuntime = vi.fn(async () => undefined);

    render(
      <FleetDashboard
        addingPeer={false}
        bootstrap={bootstrap}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={vi.fn()}
        onPairBearer={vi.fn()}
        onFetchPeerStatus={vi.fn()}
        onInitLocalRuntime={onInitLocalRuntime}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("fleet-connect-local"));

    await waitFor(() => {
      expect(onInitLocalRuntime).toHaveBeenCalledWith("local-agent");
    });
  });

  it("discovers peer connection details from a server /status address", async () => {
    const onFetchPeerStatus = vi.fn(async () => ({
      agent_name: "worker-a",
      agent_did: "did:key:z6MkWorkerA",
      desktop_graphql: "http://127.0.0.1:9181/api/v0/graphql",
      p2p: {
        p2p_shareable_address: "/ip4/100.73.235.39/tcp/9161/p2p/12D3KooWorker",
      },
    }));
    const onAddPeer = vi.fn(async () => undefined);

    render(
      <FleetDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={onAddPeer}
        onPairBearer={vi.fn()}
        onFetchPeerStatus={onFetchPeerStatus}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByTestId("fleet-add-server-address"), {
      target: { value: "http://127.0.0.1:9181" },
    });
    fireEvent.click(screen.getByTestId("fleet-add-submit"));

    await waitFor(() => {
      expect(onFetchPeerStatus).toHaveBeenCalledWith("http://127.0.0.1:9181");
      expect(onAddPeer).toHaveBeenCalledWith({
        label: "worker-a",
        agentDid: "did:key:z6MkWorkerA",
        addr: "/ip4/100.73.235.39/tcp/9161/p2p/12D3KooWorker",
        graphql: "http://127.0.0.1:9181/api/v0/graphql",
      });
    });
  });

  it("lets users preview discovered /status details before adding", async () => {
    const onFetchPeerStatus = vi.fn(async () => ({
      agent_name: "api-gateway",
      agent_did: "did:key:z6MkGateway",
      p2p_shareable_address: "iroh://gateway",
    }));

    render(
      <FleetDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={vi.fn()}
        onPairBearer={vi.fn()}
        onFetchPeerStatus={onFetchPeerStatus}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
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
    });
  });

  it("surfaces a P2P-disabled discovery result beside the fetch controls", async () => {
    const onFetchPeerStatus = vi.fn(async () => ({
      agent_name: "amy",
      agent_did: "did:key:z6MkAmy",
      p2p_transport: "none",
      p2p: {
        enabled: false,
        p2p_listen_addresses: [],
      },
    }));

    render(
      <FleetDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={vi.fn()}
        onPairBearer={vi.fn()}
        onFetchPeerStatus={onFetchPeerStatus}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
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
    const onFetchPeerStatus = vi.fn();
    const onAddPeer = vi.fn(async () => undefined);

    render(
      <FleetDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={onAddPeer}
        onPairBearer={vi.fn()}
        onFetchPeerStatus={onFetchPeerStatus}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={vi.fn()}
      />,
    );

    fireEvent.change(screen.getByTestId("fleet-add-server-address"), {
      target: { value: "http://100.73.235.38:9181/api/v0/graphql" },
    });
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
      expect(onFetchPeerStatus).not.toHaveBeenCalled();
      expect(onAddPeer).toHaveBeenCalledWith({
        label: "studio-1-steward",
        agentDid: "did:key:z6MkStudio",
        addr: "/ip4/100.73.235.38/tcp/9161/p2p/12D3KooStudio",
        graphql: "http://100.73.235.38:9181/api/v0/graphql",
      });
    });
  });
});

describe("FleetDashboard fleet-level P2P repair", () => {
  function renderFleet(deployments: DeploymentView[], onRepairP2P = vi.fn()) {
    render(
      <FleetDashboard
        addingPeer={false}
        bootstrap={bootstrap}
        deployments={deployments}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onAddPeer={vi.fn()}
        onPairBearer={vi.fn()}
        onFetchPeerStatus={vi.fn()}
        onInitLocalRuntime={vi.fn()}
        onOpenChat={vi.fn()}
        onOpenConfig={vi.fn()}
        onRepairP2P={onRepairP2P}
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
