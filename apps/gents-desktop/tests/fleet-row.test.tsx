import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { FleetRow, type FleetRowProps } from "@source-inc/gents-desktop-fleet";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { deployment } from "./config-panel-wiring/fixtures";

function renderRow(
  overrides: Partial<FleetRowProps> = {},
  dep: DeploymentView = deployment,
) {
  const props: FleetRowProps = {
    bootstrap: null,
    deployment: dep,
    onOpenChat: vi.fn(),
    onOpenConfig: vi.fn(),
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
  it("renders the chat and config action buttons keyed by peerId", () => {
    renderRow();
    expect(screen.getByTestId("fleet-chat-peer-1")).toBeInTheDocument();
    expect(screen.getByTestId("fleet-config-peer-1")).toBeInTheDocument();
    // P2P repair is a desktop-client-wide action; it must not masquerade as a
    // per-agent row action (it lives in the fleet header instead).
    expect(screen.queryByTestId("fleet-repair-peer-1")).not.toBeInTheDocument();
  });

  it("calls onOpenChat with the agent DID when the chat button is clicked", () => {
    const props = renderRow();
    fireEvent.click(screen.getByTestId("fleet-chat-peer-1"));
    expect(props.onOpenChat).toHaveBeenCalledWith(deployment.agentDid);
  });

  it("keeps chat disabled while signed bearer readiness is pending", () => {
    const props = renderRow(
      {},
      {
        ...deployment,
        source: "bearer-pairing",
        pairingReady: false,
      },
    );

    expect(screen.getByTestId("fleet-status-peer-1")).toHaveTextContent("Pairing");
    expect(screen.getByTestId("fleet-chat-peer-1")).toBeDisabled();
    fireEvent.click(screen.getByTestId("fleet-chat-peer-1"));
    expect(props.onOpenChat).not.toHaveBeenCalled();
  });

  it("calls onOpenConfig with the agent DID when the config button is clicked", () => {
    const props = renderRow();
    fireEvent.click(screen.getByTestId("fleet-config-peer-1"));
    expect(props.onOpenConfig).toHaveBeenCalledWith(deployment.agentDid);
  });

  it("shows the deployment's own runtime heartbeat as Last update", () => {
    renderRow(
      {},
      {
        ...deployment,
        runtime: { updatedAt: new Date(Date.now() - 5_000).toISOString() },
      },
    );
    expect(screen.getByTitle(/Last runtime state change/)).toHaveTextContent(/s ago/);
  });

  it("shows unknown when the deployment has no runtime heartbeat", () => {
    renderRow({}, { ...deployment, runtime: null });
    expect(screen.getByTitle(/Last runtime state change/)).toHaveTextContent("unknown");
  });

  it("claims the local init.json tool ceiling only for local-runtime rows", () => {
    const bootstrap = { initToolCeiling: "readonly" };
    renderRow(
      { bootstrap: bootstrap as FleetRowProps["bootstrap"] },
      { ...deployment, source: "local-standard" },
    );
    expect(document.querySelector('[title*="Server ceiling"]')).not.toBeNull();
  });

  it("omits the local tool ceiling from remote rows", () => {
    const bootstrap = { initToolCeiling: "readonly" };
    renderRow(
      { bootstrap: bootstrap as FleetRowProps["bootstrap"] },
      { ...deployment, source: "server-status" },
    );
    expect(document.querySelector('[title*="Server ceiling"]')).toBeNull();
  });
});
