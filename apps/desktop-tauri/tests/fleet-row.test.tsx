import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { FleetRow, type FleetRowProps } from "../src/components/fleet/FleetRow";
import type { DeploymentView } from "../src/lib/types";
import { deployment } from "./config-panel-wiring/fixtures";

function renderRow(
  overrides: Partial<FleetRowProps> = {},
  dep: DeploymentView = deployment,
) {
  const props: FleetRowProps = {
    bootstrap: null,
    deployment: dep,
    p2pHealth: null,
    repairingP2P: false,
    onOpenChat: vi.fn(),
    onOpenConfig: vi.fn(),
    onRepairP2P: vi.fn(async () => undefined),
    ...overrides,
  };
  render(
    <table>
      <tbody>
        <FleetRow {...props} />
      </tbody>
    </table>,
  );
  return props;
}

describe("FleetRow", () => {
  it("renders the three action buttons keyed by peerId", () => {
    renderRow();
    expect(screen.getByTestId("fleet-chat-peer-1")).toBeInTheDocument();
    expect(screen.getByTestId("fleet-config-peer-1")).toBeInTheDocument();
    expect(screen.getByTestId("fleet-repair-peer-1")).toBeInTheDocument();
  });

  it("calls onOpenChat with the agent DID when the chat button is clicked", () => {
    const props = renderRow();
    fireEvent.click(screen.getByTestId("fleet-chat-peer-1"));
    expect(props.onOpenChat).toHaveBeenCalledWith(deployment.agentDid);
  });

  it("calls onOpenConfig with the agent DID when the config button is clicked", () => {
    const props = renderRow();
    fireEvent.click(screen.getByTestId("fleet-config-peer-1"));
    expect(props.onOpenConfig).toHaveBeenCalledWith(deployment.agentDid);
  });

  it("disables repair when the peer dialed successfully with no error", () => {
    renderRow({}, { ...deployment, dialSucceeded: true, lastError: null });
    expect(screen.getByTestId("fleet-repair-peer-1")).toBeDisabled();
  });

  it("enables repair and fires onRepairP2P (no args) when the peer has not dialed", () => {
    const props = renderRow({}, { ...deployment, dialSucceeded: false });
    const repair = screen.getByTestId("fleet-repair-peer-1");
    expect(repair).toBeEnabled();
    fireEvent.click(repair);
    expect(props.onRepairP2P).toHaveBeenCalledTimes(1);
    // Repair is fired with NO args (unlike chat/config which pass the DID).
    expect(props.onRepairP2P).toHaveBeenCalledWith();
  });

  it("enables repair when the peer dialed but reported a last error", () => {
    renderRow({}, { ...deployment, dialSucceeded: true, lastError: "dial timeout" });
    expect(screen.getByTestId("fleet-repair-peer-1")).toBeEnabled();
  });
});
