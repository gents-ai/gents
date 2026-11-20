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
  it("keeps desktop actions available while the responsive layout hides them", () => {
    renderRow();
    expect(screen.getByTestId(`fleet-chat-${deployment.nodeDid}`)).toBeInTheDocument();
    expect(
      screen.getByTestId(`fleet-config-${deployment.nodeDid}`),
    ).toBeInTheDocument();
    expect(
      screen.queryByTestId(`fleet-repair-${deployment.nodeDid}`),
    ).not.toBeInTheDocument();
  });

  it("calls onOpenChat with the node DID when the chat button is clicked", () => {
    const props = renderRow();
    fireEvent.click(screen.getByTestId(`fleet-chat-${deployment.nodeDid}`));
    expect(props.onOpenChat).toHaveBeenCalledWith(deployment.nodeDid);
  });

  it("opens node details from the whole card and summarizes its documents", () => {
    const props = renderRow();

    expect(screen.getByTestId(`fleet-summary-${deployment.nodeDid}`)).toHaveTextContent(
      `${deployment.agents.length} agents`,
    );
    expect(screen.getByTestId(`fleet-summary-${deployment.nodeDid}`)).toHaveTextContent(
      `${deployment.sessions.length} sessions`,
    );
    fireEvent.click(screen.getByTestId(`fleet-row-${deployment.nodeDid}`));

    expect(props.onOpenChat).toHaveBeenCalledWith(deployment.nodeDid);
  });

  it("keeps chat available when pairing is pending but the runtime is connected", () => {
    const props = renderRow(
      {},
      {
        ...deployment,
        local: false,
        chatSafe: false,
      },
    );

    expect(screen.getByTestId(`fleet-status-${deployment.nodeDid}`)).toHaveTextContent(
      "Online",
    );
    expect(screen.getByTestId(`fleet-chat-${deployment.nodeDid}`)).toBeEnabled();
    fireEvent.click(screen.getByTestId(`fleet-chat-${deployment.nodeDid}`));
    expect(props.onOpenChat).toHaveBeenCalledWith(deployment.nodeDid);
  });

  it("opens retained chat while the runtime reconnects", () => {
    const props = renderRow(
      {},
      { ...deployment, local: false, runtimeConnection: "connecting" },
    );

    expect(screen.getByTestId(`fleet-status-${deployment.nodeDid}`)).toHaveTextContent(
      "Connecting",
    );
    expect(screen.getByTestId(`fleet-chat-${deployment.nodeDid}`)).toBeEnabled();
    fireEvent.click(screen.getByTestId(`fleet-chat-${deployment.nodeDid}`));
    expect(props.onOpenChat).toHaveBeenCalledWith(deployment.nodeDid);
  });

  it("calls onOpenConfig with the node DID when the config button is clicked", () => {
    const props = renderRow();
    fireEvent.click(screen.getByTestId(`fleet-config-${deployment.nodeDid}`));
    expect(props.onOpenConfig).toHaveBeenCalledWith(deployment.nodeDid);
    expect(props.onOpenChat).not.toHaveBeenCalled();
  });

  it("exposes and commits an explicit saved-label rename", () => {
    const onRenameRuntime = vi.fn();
    renderRow({ onRenameRuntime });

    const rename = screen.getByTestId(`fleet-rename-${deployment.nodeDid}`);
    expect(rename).toHaveAccessibleName("Rename Local Agent");
    expect(rename.parentElement).toHaveClass("fleet-agent-cell");
    fireEvent.click(rename);
    const input = screen.getByTestId(`fleet-rename-input-${deployment.nodeDid}`);
    fireEvent.change(input, { target: { value: "Amy" } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onRenameRuntime).toHaveBeenCalledWith(deployment.nodeDid, "Amy");
  });

  it("shows the deployment's own runtime heartbeat as Last update", () => {
    renderRow(
      {},
      {
        ...deployment,
        runtime: {
          updatedAt: new Date(Date.now() - 5_000).toISOString(),
        } as DeploymentView["runtime"],
      },
    );
    expect(screen.getByTitle(/Last runtime state change/)).toHaveTextContent(/s ago/);
  });

  it("shows unknown when the deployment has no runtime heartbeat", () => {
    renderRow({}, { ...deployment, runtime: null });
    expect(screen.getByTitle(/Last runtime state change/)).toHaveTextContent("unknown");
  });

  it("shows inference setup status only on the affected deployment row", () => {
    const onSetupInference = vi.fn();
    const missingInference: DeploymentView = {
      ...deployment,
      inferenceBackends: [],
      nodeReadiness: {
        ...deployment.nodeReadiness,
        agents: [
          {
            state: "unavailable",
            agentDid: "default",
            reason: "backend_not_configured",
          },
          { state: "ready", agentDid: "ops" },
        ],
      },
    };
    renderRow({ onSetupInference }, missingInference);

    fireEvent.click(screen.getByTestId(`fleet-inference-setup-${deployment.nodeDid}`));
    expect(onSetupInference).toHaveBeenCalledWith(missingInference);
  });

  it("does not infer missing backend setup from redacted enrolled-node config", () => {
    const onSetupInference = vi.fn();
    const hostManagedMissingCredentials: DeploymentView = {
      ...deployment,
      operatorAccess: false,
      local: false,
      inferenceBackends: [],
      nodeReadiness: {
        ...deployment.nodeReadiness,
        agents: [
          {
            state: "unavailable",
            agentDid: "default",
            reason: "credentials_required",
          },
        ],
      },
    };
    renderRow({ onSetupInference }, hostManagedMissingCredentials);

    expect(
      screen.queryByTestId(`fleet-inference-setup-${deployment.nodeDid}`),
    ).not.toBeInTheDocument();
    expect(screen.getByText("host-managed")).toBeInTheDocument();
  });

  it("claims the local init.json tool ceiling only for local-runtime rows", () => {
    const bootstrap = { initToolCeiling: "readonly" };
    renderRow(
      { bootstrap: bootstrap as FleetRowProps["bootstrap"] },
      { ...deployment, local: true },
    );
    expect(document.querySelector('[title*="Server ceiling"]')).not.toBeNull();
  });

  it("omits the local tool ceiling from remote rows", () => {
    const bootstrap = { initToolCeiling: "readonly" };
    renderRow(
      { bootstrap: bootstrap as FleetRowProps["bootstrap"] },
      { ...deployment, local: false },
    );
    expect(document.querySelector('[title*="Server ceiling"]')).toBeNull();
  });
});
