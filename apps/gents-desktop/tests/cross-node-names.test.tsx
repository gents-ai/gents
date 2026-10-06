import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { MailboxItemView, SessionSummary } from "@source-inc/gents-desktop-client";

import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { NodeBehaviorStack } from "../src/ui/screens/NodeBehaviorStack";
import { node, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const behavior = (behaviorId: string, displayName: string) => ({
  ...deployment.behaviors[0],
  behaviorId,
  displayName,
});

const summary = (over: Partial<SessionSummary>) =>
  ({
    sessionId: "session",
    agentDid: "did:key:here",
    behaviorId: null,
    title: null,
    turnState: null,
    updatedAt: null,
    ...over,
  }) as SessionSummary;

/* two nodes, the first selected; the second runs the reviewer */
function twoNodes(remote: Record<string, unknown> = {}) {
  return testApp({
    deployments: [
      node({ agentDid: "did:key:here", behaviors: [behavior("default", "Default")] }),
      node({
        agentDid: "did:key:there",
        behaviors: [behavior("reviewer", "Code Reviewer")],
        ...remote,
      }),
    ],
    selection: { agentDid: "did:key:here" },
  });
}

describe("a behavior on another node", () => {
  it("is named by the node that runs it, not the selected one", () => {
    renderIn(
      twoNodes(),
      <NodeBehaviorStack nodeDid="did:key:there" behaviorId="reviewer" keyboard />,
    );
    expect(
      screen.getByRole("button", { name: "About Code Reviewer behavior" }),
    ).toBeInTheDocument();
  });

  it("names a worker's behavior by the worker's node", () => {
    const worker = summary({
      sessionId: "worker",
      agentDid: "did:key:there",
      behaviorId: "reviewer",
    });
    renderIn(
      twoNodes(),
      <NodeBehaviorStack
        nodeDid="did:key:here"
        behaviorId="default"
        workers={[worker]}
      />,
    );
    expect(screen.getByText("Cr")).toBeInTheDocument();
  });

  it("titles a mailbox item's session from the node that lists it", () => {
    const item = {
      itemId: "item-1",
      itemKey: "key-1",
      requesterDid: "did:key:person",
      agentDid: "did:key:there",
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
      targetAgentDid: "did:key:there",
      targetBehaviorId: "reviewer",
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
            agentDid: "did:key:there",
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
