import { act } from "@testing-library/react";
import { Profiler } from "react";
import { describe, expect, it } from "vitest";

import type { MailboxItemView, SessionSummary } from "@source-inc/gents-desktop-client";

import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { node, publish, renderIn, testApp } from "./app-fixture";

const AGENT = "did:key:here";

const summary = (sessionId: string, title: string) =>
  ({
    sessionId,
    agentDid: AGENT,
    requesterDid: null,
    behaviorId: null,
    title,
    turnState: null,
    updatedAt: null,
  }) as SessionSummary;

const item = (itemId: string): MailboxItemView => ({
  itemId,
  itemKey: itemId,
  requesterDid: "did:key:person",
  agentDid: AGENT,
  status: "open",
  kind: "ask",
  action: "reply",
  title: "A question",
  summary: null,
  payload: null,
  sourceKind: "session",
  sourceId: "s-1",
  sessionId: "s-1",
  requestId: null,
  graphRunId: null,
  causeDocId: null,
  targetAgentDid: AGENT,
  targetBehaviorId: "default",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: "2026-10-06T00:00:00.000Z",
});

const here = (over: Record<string, unknown> = {}) =>
  node({ agentDid: AGENT, sessions: [summary("s-1", "one")], ...over });

/* renders `ui` under `app` and counts the commits that reach it */
function commitsOf(app: ReturnType<typeof testApp>, ui: React.ReactElement) {
  let commits = 0;
  renderIn(
    app,
    <Profiler id="probe" onRender={() => (commits += 1)}>
      {ui}
    </Profiler>,
  );
  return () => commits;
}

describe("screens read the fleet by what they show", () => {
  it("a mailbox item arriving does not re-render the sessions list", () => {
    const app = testApp({ deployments: [here()], selection: { agentDid: AGENT } });
    const commits = commitsOf(app, <SessionsScreen />);
    const before = commits();
    act(() => publish(app, [here({ mailboxItems: [item("m-1")] })]));
    expect(commits()).toBe(before);
  });

  it("a session in scope changing re-renders the sessions list", () => {
    const app = testApp({ deployments: [here()], selection: { agentDid: AGENT } });
    const commits = commitsOf(app, <SessionsScreen />);
    const before = commits();
    act(() => publish(app, [here({ sessions: [summary("s-1", "renamed")] })]));
    expect(commits()).toBeGreaterThan(before);
  });

  it("a session no item names changing does not re-render the mailbox", () => {
    const app = testApp({
      deployments: [here({ mailboxItems: [item("m-1")] })],
      selection: { agentDid: AGENT },
    });
    const commits = commitsOf(app, <MailboxScreen />);
    const before = commits();
    act(() =>
      publish(app, [
        here({
          mailboxItems: [item("m-1")],
          sessions: [summary("s-1", "one"), summary("s-2", "elsewhere")],
        }),
      ]),
    );
    expect(commits()).toBe(before);
  });
});
