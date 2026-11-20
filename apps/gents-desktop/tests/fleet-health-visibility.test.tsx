import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { FleetRow, type FleetRowProps } from "@source-inc/gents-desktop-fleet";
import { deploymentStatus } from "@source-inc/gents-desktop-fleet";
import {
  projectRuntimeConnectionStatus,
  type DeploymentView,
  type SyncHealthView,
} from "@source-inc/gents-desktop-client";
import { deployment } from "./config-panel-wiring/fixtures";

function syncHealth(overrides: Partial<SyncHealthView> = {}): SyncHealthView {
  return {
    state: "healthy",

    runtimeCount: 1,
    connectedRuntimeCount: 1,

    cause: null,
    nextAction: null,
    ...overrides,
  };
}

function renderRow(dep: DeploymentView, sync = syncHealth()) {
  const props: FleetRowProps = {
    bootstrap: null,
    deployment: dep,
    syncHealth: sync,
    onOpenChat: vi.fn(),
    onOpenConfig: vi.fn(),
  };
  render(
    <table>
      <tbody>
        <FleetRow {...props} />
      </tbody>
    </table>,
  );
}

describe("fleet health visibility", () => {
  it("labels statuses instead of relying on a hover dot", () => {
    expect(deploymentStatus({ ...deployment }).label).toBe("Online");
    expect(deploymentStatus({ ...deployment }, syncHealth()).label).toBe("Online");
    // The label is the presentation owner's (its reason is checked against
    // Lean in lean-conformance/runtime-connection); an outage never reads as
    // a connection attempt.
    const outage = deploymentStatus({
      ...deployment,
      runtimeConnection: "disconnected",
    }).label;
    expect(outage).toBe(projectRuntimeConnectionStatus("disconnected").shortLabel);
    expect(outage).not.toBe(projectRuntimeConnectionStatus("connecting").shortLabel);
    expect(
      deploymentStatus({
        ...deployment,
      }).label,
    ).toBe("Online");
  });

  it("keeps enrolled chat available when current readiness has an old observation timestamp", () => {
    const stale = {
      ...deployment,
      local: false,

      nodeReadiness: {
        ...deployment.nodeReadiness,
        source: { state: "current" as const },
        updatedAt: "2000-01-01T00:00:00Z",
      },
    };
    const syncing = syncHealth({
      state: "waiting",
      cause: "discovery_degraded",
      nextAction: "wait",
    });
    expect(deploymentStatus(stale, syncing).label).toBe("Online");
    renderRow(stale, syncing);
    expect(screen.getByTestId(`fleet-chat-${deployment.nodeDid}`)).toBeEnabled();
  });

  it("shows the runtime refusal instead of claiming the node is online", () => {
    renderRow({ ...deployment, runtimeConnection: "refused" });
    expect(screen.getByTestId(`fleet-status-${deployment.nodeDid}`)).toHaveTextContent(
      "Refused",
    );
    expect(screen.getByTestId(`fleet-chat-${deployment.nodeDid}`)).toBeDisabled();
  });

  it("shows useful document counts instead of transport identifiers", () => {
    renderRow({ ...deployment });
    expect(screen.getByTestId(`fleet-summary-${deployment.nodeDid}`)).toHaveTextContent(
      "2 agents · 0 sessions · 2 tasks",
    );
    expect(screen.queryByRole("button", { name: "Copy DID" })).not.toBeInTheDocument();
    expect(screen.queryByText(/GraphQL/)).not.toBeInTheDocument();
    expect(
      screen.queryByTestId(`fleet-error-${deployment.nodeDid}`),
    ).not.toBeInTheDocument();
  });

  it("keeps the fleet actions focused on sessions and configuration", () => {
    renderRow({ ...deployment });
    expect(screen.getByTestId(`fleet-chat-${deployment.nodeDid}`)).toBeInTheDocument();
    expect(
      screen.getByTestId(`fleet-config-${deployment.nodeDid}`),
    ).toBeInTheDocument();
    expect(
      screen.queryByTestId(`fleet-code-${deployment.nodeDid}`),
    ).not.toBeInTheDocument();
  });
});
