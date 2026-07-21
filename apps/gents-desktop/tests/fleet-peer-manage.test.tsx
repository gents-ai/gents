import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { FleetRow } from "../src/components/fleet/FleetRow";
import type { DeploymentView } from "../src/lib/types";

function makeDeployment(overrides: Partial<DeploymentView> = {}): DeploymentView {
  return {
    peerId: "peer-1",
    label: "Remote Agent",
    agentDid: "did:key:z6MkRemote",
    addr: "/ip4/10.0.0.2/tcp/9292",
    graphql: null,
    source: "server-status",
    agentPrincipal: { agentDid: "did:key:z6MkRemote" },
    behaviors: [],
    tasks: [],
    schedules: [],
    eventTriggers: [],
    skills: [],
    inferenceBackends: [],
    inferenceProfiles: [],
    toolSelections: [],
    toolServices: [],
    conversations: [],
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
          onRemovePeer={vi.fn()}
          onRenamePeer={vi.fn()}
          {...handlers}
        />
      </tbody>
    </table>,
  );
}

describe("fleet peer management", () => {
  it("removes a saved peer only after confirmation", async () => {
    const onRemovePeer = vi.fn().mockResolvedValue(undefined);
    renderRow(makeDeployment(), { onRemovePeer });

    fireEvent.click(screen.getByTestId("fleet-remove-peer-1"));
    expect(onRemovePeer).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(onRemovePeer).toHaveBeenCalledWith("peer-1"));
  });

  it("offers no remove action for the local runtime", () => {
    renderRow(makeDeployment({ source: "local-standard" }));
    expect(screen.queryByTestId("fleet-remove-peer-1")).not.toBeInTheDocument();
  });

  it("renames via the inline editor and skips no-op commits", () => {
    const onRenamePeer = vi.fn().mockResolvedValue(undefined);
    renderRow(makeDeployment(), { onRenamePeer });

    fireEvent.click(screen.getByTestId("fleet-rename-peer-1"));
    const input = screen.getByTestId("fleet-rename-input-peer-1");
    fireEvent.change(input, { target: { value: "Edge Rack 2" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onRenamePeer).toHaveBeenCalledWith("peer-1", "Edge Rack 2");

    fireEvent.click(screen.getByTestId("fleet-rename-peer-1"));
    const again = screen.getByTestId("fleet-rename-input-peer-1");
    fireEvent.keyDown(again, { key: "Escape" });
    expect(onRenamePeer).toHaveBeenCalledTimes(1);
  });
});
