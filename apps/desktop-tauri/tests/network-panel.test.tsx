import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { NetworkPanel } from "../src/components/fleet/NetworkPanel";
import { setDesktopApiAdapterForTests } from "../src/lib/desktop-api";
import type { DesktopApiAdapter } from "../src/lib/desktop-api";

function withStatus(status: unknown, fail = false) {
  setDesktopApiAdapterForTests({
    fetchNetworkStatus: fail
      ? vi.fn().mockRejectedValue(new Error("p2p subsystem offline"))
      : vi.fn().mockResolvedValue(status),
  } as unknown as DesktopApiAdapter);
}

describe("network panel", () => {
  afterEach(() => setDesktopApiAdapterForTests(null));

  it("stays collapsed until opened, then shows probes with peer labels", async () => {
    withStatus({
      localPeerId: "12D3KooWLocal",
      listenAddresses: ["/ip4/127.0.0.1/tcp/9292"],
      connectedPeers: ["peer-edge"],
      replicators: [
        {
          peerId: "peer-edge",
          address: "/ip4/10.0.0.2/tcp/9292",
          collections: ["AgentRequest", "AgentResponse"],
          status: 0,
          lastStatusChange: new Date(Date.now() - 3_600_000).toISOString(),
        },
      ],
      savedPeers: [
        {
          peerId: "peer-edge",
          label: "Edge Rack",
          addr: "/ip4/10.0.0.2/tcp/9292",
          agentDid: "did:key:z6MkEdge",
        },
      ],
    });
    render(<NetworkPanel />);

    expect(screen.queryByTestId("network-refresh")).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId("network-toggle"));

    await waitFor(() =>
      expect(screen.getByTestId("network-connected")).toHaveTextContent("Edge Rack"),
    );
    expect(screen.getByText("12D3KooWLocal")).toBeInTheDocument();
    expect(screen.getByText("2 collections")).toBeInTheDocument();
    expect(screen.getByText("1h ago")).toBeInTheDocument();
  });

  it("renders per-probe errors without hiding healthy probes", async () => {
    withStatus({
      localPeerId: "12D3KooWLocal",
      listenAddresses: [],
      listenAddressesError: "timed out reading desktop P2P listen addresses",
      connectedPeers: [],
      replicators: [],
      savedPeers: [],
    });
    render(<NetworkPanel />);
    fireEvent.click(screen.getByTestId("network-toggle"));

    await waitFor(() =>
      expect(
        screen.getByText("timed out reading desktop P2P listen addresses"),
      ).toBeInTheDocument(),
    );
    expect(screen.getByText("12D3KooWLocal")).toBeInTheDocument();
  });

  it("surfaces a whole-fetch failure with retry", async () => {
    withStatus(null, true);
    render(<NetworkPanel />);
    fireEvent.click(screen.getByTestId("network-toggle"));

    await waitFor(() =>
      expect(screen.getByTestId("network-error")).toHaveTextContent(
        "p2p subsystem offline",
      ),
    );
    expect(screen.getByTestId("network-refresh")).toBeEnabled();
  });
});
