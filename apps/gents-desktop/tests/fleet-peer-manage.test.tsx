import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { FleetRow } from "@source-inc/gents-desktop-fleet";
import type { DeploymentView } from "@source-inc/gents-desktop-client";

function makeDeployment(overrides: Partial<DeploymentView> = {}): DeploymentView {
  return {
    label: "Remote Node",
    nodeDid: "did:key:z6MkRemote",

    graphql: null,
    local: false,

    runtimeConnection: "connected",
    chatSafe: true,

    node: { nodeDid: "did:key:z6MkRemote" },
    nodeConfig: null,
    nodeReadiness: {
      source: { state: "current" },
      activeGeneration: 1,
      routerGeneration: 1,
      updatedAt: "2026-09-02T00:00:00Z",
      agents: [],
    },
    agents: [],
    agentConfigs: [],
    agentEnvironments: [],
    tasks: [],
    schedules: [],
    eventSources: [],
    triggers: [],
    skills: [],
    inferenceBackends: [],
    inferenceProfiles: [],
    inferenceSampling: [],
    inferenceExecution: [],
    contexts: [],
    compactions: [],
    tools: [],
    toolServiceRegistries: [],
    agentTargets: [],
    datastoreToolSurfaces: [],
    sessions: [],
    mailboxItems: [],
    runtime: null,
    ...overrides,
  } as DeploymentView;
}

function renderRow(deployment: DeploymentView, handlers: Record<string, unknown> = {}) {
  return render(
    <table>
      <tbody>
        <FleetRow
          bootstrap={null}
          deployment={deployment}
          onOpenChat={vi.fn()}
          onOpenConfig={vi.fn()}
          onRemoveRuntime={vi.fn()}
          onRenameRuntime={vi.fn()}
          {...handlers}
        />
      </tbody>
    </table>,
  );
}

describe("fleet peer management", () => {
  it("removes a saved peer only after confirmation", async () => {
    const onRemoveRuntime = vi.fn().mockResolvedValue(undefined);
    renderRow(makeDeployment(), { onRemoveRuntime });

    fireEvent.click(screen.getByTestId("fleet-remove-did:key:z6MkRemote"));
    expect(onRemoveRuntime).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    await waitFor(() =>
      expect(onRemoveRuntime).toHaveBeenCalledWith("did:key:z6MkRemote"),
    );
  });

  it("offers no remove action for the local runtime", () => {
    renderRow(makeDeployment({ local: true }));
    expect(
      screen.queryByTestId("fleet-remove-did:key:z6MkRemote"),
    ).not.toBeInTheDocument();
  });

  it("renames via the inline editor and skips no-op commits", () => {
    const onRenameRuntime = vi.fn().mockResolvedValue(undefined);
    renderRow(makeDeployment(), { onRenameRuntime });

    fireEvent.click(screen.getByTestId("fleet-rename-did:key:z6MkRemote"));
    const input = screen.getByTestId("fleet-rename-input-did:key:z6MkRemote");
    fireEvent.change(input, { target: { value: "Edge Rack 2" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onRenameRuntime).toHaveBeenCalledWith("did:key:z6MkRemote", "Edge Rack 2");

    fireEvent.click(screen.getByTestId("fleet-rename-did:key:z6MkRemote"));
    const again = screen.getByTestId("fleet-rename-input-did:key:z6MkRemote");
    fireEvent.keyDown(again, { key: "Escape" });
    expect(onRenameRuntime).toHaveBeenCalledTimes(1);
  });
});
