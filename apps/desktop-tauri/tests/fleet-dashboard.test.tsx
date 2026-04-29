import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { FleetDashboard } from "../src/components/fleet/FleetDashboard";

describe("FleetDashboard add connection flow", () => {
  it("discovers peer connection details from a server /status address", async () => {
    const onFetchPeerStatus = vi.fn(async () => ({
      agent_name: "worker-a",
      agent_did: "did:key:z6MkWorkerA",
      p2p: {
        p2p_shareable_address:
          "/ip4/100.73.235.39/tcp/9161/p2p/12D3KooWorker",
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
        onFetchPeerStatus={onFetchPeerStatus}
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
        onFetchPeerStatus={onFetchPeerStatus}
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
    });
  });
});
