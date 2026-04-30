import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { Sidebar } from "../src/components/Sidebar";
import type { BehaviorView, DeploymentView } from "../src/lib/types";

function deployment(peerId: string, label: string, agentDid: string): DeploymentView {
  return {
    peerId,
    label,
    agentDid,
    addr: `iroh://${peerId}`,
    dialSucceeded: true,
    agentPrincipal: {
      agentDid,
    },
    behaviors: [],
    inferenceBackends: [],
    inferenceProfiles: [],
    toolSelections: [],
    toolServiceRegistries: [],
    tasks: [],
    schedules: [],
    eventTriggers: [],
    conversations: [],
  };
}

function behavior(behaviorId: string, displayName: string): BehaviorView {
  return {
    behaviorId,
    displayName,
    enabled: true,
    isDefault: false,
  };
}

describe("Sidebar", () => {
  it("shows only the selected peer and routes fleet/config actions", () => {
    const selected = deployment("peer-1", "mini-1-steward", "did:key:z6Mini1");
    const hidden = deployment("peer-2", "mini-2-steward", "did:key:z6Mini2");
    const onOpenFleet = vi.fn();
    const onConfigureDeployment = vi.fn();

    render(
      <Sidebar
        behaviorOptions={[]}
        conversations={[]}
        deployments={[selected, hidden]}
        selectedAgentDid={selected.agentDid}
        selectedBehaviorId={null}
        selectedSessionId={null}
        onConfigureDeployment={onConfigureDeployment}
        onOpenFleet={onOpenFleet}
        onSelectBehavior={vi.fn()}
        onSelectSession={vi.fn()}
        onStartNewConversation={vi.fn()}
      />,
    );

    expect(screen.getAllByText("mini-1-steward")).toHaveLength(2);
    expect(screen.queryByText("mini-2-steward")).toBeNull();

    fireEvent.click(screen.getByText("Fleet Dashboard"));
    expect(onOpenFleet).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByText("Configure"));
    expect(onConfigureDeployment).toHaveBeenCalledWith(selected.agentDid);
  });

  it("starts a blank conversation for a behavior from the behavior list", () => {
    const selected = deployment("peer-1", "mini-1-steward", "did:key:z6Mini1");
    const stewardBehavior = behavior("steward", "Mini 1 Host Steward");
    const onStartNewConversation = vi.fn();
    const onSelectBehavior = vi.fn();

    render(
      <Sidebar
        behaviorOptions={[stewardBehavior]}
        conversations={[]}
        deployments={[selected]}
        selectedAgentDid={selected.agentDid}
        selectedBehaviorId={stewardBehavior.behaviorId}
        selectedSessionId={null}
        onConfigureDeployment={vi.fn()}
        onOpenFleet={vi.fn()}
        onSelectBehavior={onSelectBehavior}
        onSelectSession={vi.fn()}
        onStartNewConversation={onStartNewConversation}
      />,
    );

    fireEvent.click(screen.getByTestId("sidebar-new-chat-steward"));
    expect(onStartNewConversation).toHaveBeenCalledWith("steward");
    expect(onSelectBehavior).not.toHaveBeenCalled();
  });
});
