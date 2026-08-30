import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { FleetHostDashboard } from "../src/components/fleet/FleetHostDashboard";
import type {
  BootstrapSummary,
  DeploymentView,
  EnrollmentRequestView,
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

const enrollmentRequest: EnrollmentRequestView = {
  requestId: "enrollment-request-1",
  networkId: "network-amy",
  adminDid: "did:key:z6MkAmy",
  serverPeer: "server-peer-amy",
  ownerAgent: "did:key:z6MkAmy",
  state: "pending",
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
        onRequestStatusEnrollment={vi.fn()}
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

  it("requests authenticated enrollment from a server /status address", async () => {
    const onRequestStatusEnrollment = vi.fn(async () => enrollmentRequest);

    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onRequestStatusEnrollment={onRequestStatusEnrollment}
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
      expect(onRequestStatusEnrollment).toHaveBeenCalledWith("http://127.0.0.1:9181");
      expect(screen.getByTestId("fleet-import-status")).toHaveTextContent(
        "awaiting server approval",
      );
    });
  });

  it("contains and renders a rejected enrollment request", async () => {
    const onRequestStatusEnrollment = vi.fn(async () => {
      throw new Error("enrollment rejected");
    });

    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onRequestStatusEnrollment={onRequestStatusEnrollment}
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
      expect(screen.getByTestId("fleet-import-status")).toHaveTextContent(
        "enrollment rejected",
      );
    });
  });

  it("surfaces an unavailable enrollment offer beside the controls", async () => {
    const onRequestStatusEnrollment = vi.fn(async () => {
      throw new Error("This runtime has P2P disabled");
    });

    render(
      <FleetHostDashboard
        addingPeer={false}
        bootstrap={null}
        deployments={[]}
        loading={false}
        p2pHealth={null}
        repairingP2P={false}
        starting={false}
        onRequestStatusEnrollment={onRequestStatusEnrollment}
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
    expect(screen.getByTestId("fleet-add-server-address")).toHaveValue(
      "http://amy.local:9191",
    );
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
        onRequestStatusEnrollment={vi.fn()}
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
