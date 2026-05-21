import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ConversationListSection } from "../src/components/sidebar-widgets/ConversationListSection";
import type { ConversationSummary, DeploymentView, TaskView } from "../src/lib/types";

function task(taskId: string, name: string): TaskView {
  return {
    taskId,
    name,
    enabled: true,
    recentRuns: {
      totalFires: 0,
      scheduleCount: 0,
      eventTriggerCount: 0,
    },
    runHistory: [],
  };
}

const deployment: DeploymentView = {
  peerId: "peer-1",
  label: "mini-1-steward",
  agentDid: "did:key:z6Mini",
  addr: "iroh://mini-1",
  dialSucceeded: true,
  agentPrincipal: {
    agentDid: "did:key:z6Mini",
  },
  behaviors: [],
  inferenceBackends: [],
  inferenceProfiles: [],
  toolSelections: [],
  toolServiceRegistries: [],
  tasks: [task("freshness", "Freshness check"), task("drift", "Drift report")],
  schedules: [],
  eventTriggers: [],
  conversations: [],
};

const conversations: ConversationSummary[] = [
  {
    sessionId: "session-fresh",
    title: "freshness-run",
    taskId: "freshness",
    taskName: "Freshness check",
    messageCount: 4,
    toolCallCount: 1,
  },
  {
    sessionId: "session-drift",
    title: "drift-run",
    taskId: "drift",
    taskName: "Drift report",
    messageCount: 3,
    toolCallCount: 1,
  },
  {
    sessionId: "session-manual",
    title: "operator-check",
    messageCount: 2,
    toolCallCount: 0,
  },
];

describe("ConversationListSection", () => {
  it("shows task tags and filters conversations by task", async () => {
    const onSelectSession = vi.fn();

    render(
      <ConversationListSection
        conversations={conversations}
        deployments={[deployment]}
        selectedAgentDid={deployment.agentDid}
        selectedSessionId="session-drift"
        onSelectSession={onSelectSession}
      />,
    );

    expect(
      within(screen.getByTestId("conversation-session-fresh")).getByText(
        "Freshness check",
      ),
    ).toBeInTheDocument();
    expect(screen.getByTestId("conversation-session-drift")).toBeInTheDocument();

    fireEvent.change(screen.getByTestId("conversation-task-filter"), {
      target: { value: "freshness" },
    });

    expect(screen.getByTestId("conversation-session-fresh")).toBeInTheDocument();
    expect(screen.queryByTestId("conversation-session-drift")).toBeNull();
    expect(screen.queryByTestId("conversation-session-manual")).toBeNull();

    await waitFor(() => {
      expect(onSelectSession).toHaveBeenCalledWith("session-fresh");
    });
  });
});
