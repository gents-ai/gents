import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { MailboxItemView, SessionSummary } from "@source-inc/gents-desktop-client";

import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { NodeAgentStack } from "../src/ui/screens/NodeBehaviorStack";
import { node, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const agent = (agentId: string, displayName: string) => ({
  ...deployment.agents[0],
  agentId,
  displayName,
});

const summary = (over: Partial<SessionSummary>) =>
  ({
    sessionId: "session",
    nodeDid: "did:key:here",
    agentId: null,
    title: null,
    turnState: null,
    updatedAt: null,
    ...over,
  }) as SessionSummary;

/* two nodes, the first selected; the second runs the reviewer */
function twoNodes(remote: Record<string, unknown> = {}) {
  return testApp({
    deployments: [
      node({ nodeDid: "did:key:here", agents: [agent("default", "Default")] }),
      node({
        nodeDid: "did:key:there",
        agents: [agent("reviewer", "Code Reviewer")],
        ...remote,
      }),
    ],
    selection: { nodeDid: "did:key:here" },
  });
}

describe("a agent on another node", () => {
  it("is named by the node that runs it, not the selected one", () => {
    renderIn(
      twoNodes(),
      <NodeAgentStack nodeDid="did:key:there" agentId="reviewer" keyboard />,
    );
    expect(
      screen.getByRole("button", { name: "About Code Reviewer agent" }),
    ).toBeInTheDocument();
  });

  it("names a worker's agent by the worker's node", () => {
    const worker = summary({
      sessionId: "worker",
      nodeDid: "did:key:there",
      agentId: "reviewer",
    });
    renderIn(
      twoNodes(),
      <NodeAgentStack nodeDid="did:key:here" agentId="default" workers={[worker]} />,
    );
    expect(screen.getByText("Cr")).toBeInTheDocument();
  });

  it("keeps same-named agents on distinct nodes separate from the parent and each other", () => {
    const app = testApp({
      deployments: [
        node({ nodeDid: "did:key:here", agents: [agent("default", "Local Agent")] }),
        node({
          nodeDid: "did:key:there",
          agents: [agent("default", "Remote Reviewer")],
        }),
        node({ nodeDid: "did:key:other", agents: [agent("default", "Third Checker")] }),
      ],
      selection: { nodeDid: "did:key:here" },
    });
    renderIn(
      app,
      <NodeAgentStack
        nodeDid="did:key:here"
        agentId="default"
        workers={[
          summary({ sessionId: "first", nodeDid: "did:key:there", agentId: "default" }),
          summary({
            sessionId: "duplicate",
            nodeDid: "did:key:there",
            agentId: "default",
          }),
          summary({
            sessionId: "second",
            nodeDid: "did:key:other",
            agentId: "default",
          }),
        ]}
      />,
    );
    expect(screen.getByText("La")).toBeInTheDocument();
    expect(screen.getAllByText("Rr")).toHaveLength(1);
    expect(screen.getAllByText("Tc")).toHaveLength(1);
  });

  it("titles a mailbox item's session from the node that lists it", () => {
    const item = {
      itemId: "item-1",
      itemKey: "key-1",
      requesterDid: "did:key:person",
      nodeDid: "did:key:there",
      status: "open",
      kind: "finished",
      action: "ack",
      title: "Review done",
      summary: null,
      payload: null,
      sourceKind: "session",
      sourceId: "remote-session",
      sessionId: "remote-session",
      requestId: null,
      graphRunId: null,
      causeDocId: null,
      targetNodeDid: "did:key:there",
      targetAgentId: "reviewer",
      expectedCollection: null,
      parentItemId: null,
      deadlineAt: null,
      createdAt: new Date().toISOString(),
    } satisfies MailboxItemView;
    renderIn(
      twoNodes({
        mailboxItems: [item],
        sessions: [
          summary({
            sessionId: "remote-session",
            nodeDid: "did:key:there",
            title: "Diff review",
          }),
        ],
      }),
      <MailboxScreen />,
    );
    expect(screen.getByText("in Diff review")).toBeInTheDocument();
    expect(screen.getByText("Code Reviewer")).toBeInTheDocument();
  });
});
