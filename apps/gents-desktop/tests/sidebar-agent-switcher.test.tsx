import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ConnectedPeerSection } from "../src/components/sidebar-widgets/ConnectedPeerSection";
import type { DeploymentView } from "@source-inc/gents-desktop-client";

function dep(agentDid: string, label: string): DeploymentView {
  return {
    peerId: `peer-${label}`,
    label,
    agentDid,
    agentPrincipal: { agentDid },
    behaviors: [],
    tasks: [],
    conversations: [],
  } as unknown as DeploymentView;
}

describe("sidebar agent switcher", () => {
  it("switches between deployments", () => {
    const onSelectAgent = vi.fn();
    render(
      <ConnectedPeerSection
        deployments={[dep("did:a", "Alpha"), dep("did:b", "Beta")]}
        selectedAgentDid="did:a"
        onOpenFleet={vi.fn()}
        onConfigureDeployment={vi.fn()}
        onSelectAgent={onSelectAgent}
      />,
    );

    fireEvent.change(screen.getByTestId("sidebar-agent-switcher"), {
      target: { value: "did:b" },
    });
    expect(onSelectAgent).toHaveBeenCalledWith("did:b");
  });

  it("keeps the static title for a single deployment", () => {
    render(
      <ConnectedPeerSection
        deployments={[dep("did:a", "Alpha")]}
        selectedAgentDid="did:a"
        onOpenFleet={vi.fn()}
        onConfigureDeployment={vi.fn()}
        onSelectAgent={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("sidebar-agent-switcher")).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Alpha" })).toBeInTheDocument();
  });
});
