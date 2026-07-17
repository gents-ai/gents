import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { FleetRow, type FleetRowProps } from "../src/components/fleet/FleetRow";
import { deploymentStatus } from "../src/components/fleet/fleetMetrics";
import type { DeploymentView } from "../src/lib/types";
import { deployment } from "./config-panel-wiring/fixtures";

function renderRow(dep: DeploymentView) {
  const props: FleetRowProps = {
    bootstrap: null,
    deployment: dep,
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
    expect(
      deploymentStatus({ ...deployment, dialSucceeded: true, lastError: null }).label,
    ).toBe("Online");
    expect(deploymentStatus({ ...deployment, dialSucceeded: false }).label).toBe(
      "Not connected",
    );
    expect(deploymentStatus({ ...deployment, lastError: "dial timeout" }).label).toBe(
      "Error",
    );
  });

  it("shows a visible error line with remediation-aware copy on failing rows", () => {
    renderRow({ ...deployment, dialSucceeded: true, lastError: "dial timeout" });
    expect(screen.getByTestId("fleet-status-peer-1")).toHaveTextContent("Error");
    expect(screen.getByTestId("fleet-error-peer-1")).toHaveTextContent("dial timeout");
  });

  it("offers a DID copy affordance in the agent cell", () => {
    renderRow({ ...deployment, dialSucceeded: true, lastError: null });
    expect(screen.getByRole("button", { name: "Copy DID" })).toBeInTheDocument();
    expect(screen.queryByTestId("fleet-error-peer-1")).not.toBeInTheDocument();
  });

  it("offers a Code-mode action when wired", () => {
    const onOpenCode = vi.fn();
    render(
      <table>
        <tbody>
          <FleetRow
            bootstrap={null}
            deployment={{ ...deployment, dialSucceeded: true, lastError: null }}
            onOpenChat={vi.fn()}
            onOpenCode={onOpenCode}
            onOpenConfig={vi.fn()}
          />
        </tbody>
      </table>,
    );
    fireEvent.click(screen.getByTestId("fleet-code-peer-1"));
    expect(onOpenCode).toHaveBeenCalledWith(deployment.agentDid);
  });
});
