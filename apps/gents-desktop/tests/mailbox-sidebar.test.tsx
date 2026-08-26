import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { Sidebar } from "../src/components/Sidebar";

function item(overrides: Partial<MailboxItemView> = {}): MailboxItemView {
  return {
    itemId: "item-1",
    itemKey: "graph:wait-1:ask:1",
    requesterDid: "did:user",
    agentDid: "did:agent",
    status: "open",
    kind: "ask",
    action: "start_request",
    title: "Review the result",
    summary: "The graph is waiting for a decision.",
    payload: null,
    sourceKind: "graph",
    sourceId: "wait-1",
    sessionId: "session-1",
    requestId: null,
    graphRunId: "run-1",
    causeDocId: null,
    targetAgentDid: "did:agent",
    targetBehaviorId: "operator",
    expectedCollection: null,
    parentItemId: null,
    deadlineAt: null,
    createdAt: "2026-08-25T12:00:00Z",
    ...overrides,
  };
}

function renderSidebar(mailboxItems: MailboxItemView[]) {
  const onOpenMailboxItem = vi.fn();
  const onDismissMailboxItem = vi.fn();
  render(
    <Sidebar
      deployments={[]}
      conversations={[]}
      mailboxItems={mailboxItems}
      selectedAgentDid="did:agent"
      selectedBehaviorId="operator"
      selectedSessionId={null}
      onOpenFleet={vi.fn()}
      onConfigureDeployment={vi.fn()}
      onSelectBehavior={vi.fn()}
      onSelectSession={vi.fn()}
      onStartNewConversation={vi.fn()}
      onOpenMailboxItem={onOpenMailboxItem}
      onDismissMailboxItem={onDismissMailboxItem}
    />,
  );
  fireEvent.click(screen.getByTestId("agent-tab-mailbox"));
  return { onOpenMailboxItem, onDismissMailboxItem };
}

describe("mailbox sidebar", () => {
  it("renders open envelopes, badge, routed action, and dismiss", () => {
    const { onOpenMailboxItem, onDismissMailboxItem } = renderSidebar([item()]);
    expect(screen.getByTestId("agent-tab-mailbox")).toHaveTextContent("Mailbox (1)");
    expect(screen.getByText("Review the result")).toBeInTheDocument();
    expect(
      screen.getByText("The graph is waiting for a decision."),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("mailbox-open-item-1"));
    expect(onOpenMailboxItem).toHaveBeenCalledWith("item-1");
    fireEvent.click(screen.getByTestId("mailbox-dismiss-item-1"));
    expect(onDismissMailboxItem).toHaveBeenCalledWith("item-1");
  });

  it("keeps ack items read-only except for dismissal", () => {
    renderSidebar([item({ action: "ack" })]);
    expect(screen.queryByText("Open compose")).not.toBeInTheDocument();
    expect(screen.getByText("Dismiss")).toBeInTheDocument();
  });
});
