import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { node, testApp, withApp } from "./app-fixture";
import type {
  LinkedSessionView,
  SessionProvenanceView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";

import { useParentWork } from "../src/ui/screens/parentWork";

const AGENT = "did:key:parent";

const session = (overrides: Partial<SessionSummary>): SessionSummary =>
  ({
    sessionId: "session",
    agentDid: AGENT,
    requesterDid: null,
    latestRequestDocId: null,
    closedAt: null,
    tags: [],
    provenance: null,
    title: null,
    previewText: null,
    status: null,
    behaviorId: null,
    latestRequestId: null,
    taskId: null,
    taskName: null,
    triggerId: null,
    triggerKind: null,
    createdAt: null,
    updatedAt: null,
    turnState: null,
    ...overrides,
  }) as SessionSummary;

const parent = session({
  sessionId: "session-parent",
  title: "Lead",
  behaviorId: "default",
  turnState: "completed",
});
const other = session({ sessionId: "session-other", title: "Reviewer" });
const child = session({ sessionId: "session-child" });

const link = (
  sessionId: string,
  requesterDid: string | null = null,
): LinkedSessionView => ({
  agentDid: AGENT,
  sessionId,
  requesterDid,
  causeRequestDocId: `doc-${sessionId}`,
});

const view = (
  startedBy: LinkedSessionView | null,
  senders: [string, LinkedSessionView][] = [],
): SessionProvenanceView => ({
  sessionId: "session-child",
  startedBy,
  started: [],
  sent: [],
  received: [],
  senders: senders.map(([requestId, sender]) => ({ requestId, sender })),
  calls: [],
});

/* the parent work of the child session, selected on the agent's node */
function parentWorkOf(provenance: SessionProvenanceView | null) {
  const app = testApp({
    deployments: [node({ agentDid: AGENT, sessions: [parent, other, child] })],
    selection: { agentDid: AGENT, sessionId: "session-child" },
  });
  return renderHook(() => useParentWork(provenance), { wrapper: withApp(app) });
}

describe("the sessions that sent work into this one", () => {
  it("names the session the lineage owner says started it", () => {
    const { result } = parentWorkOf(view(link("session-parent")));
    expect(result.current.parent?.sessionId).toBe("session-parent");
    expect(result.current.parent?.summary?.title).toBe("Lead");
  });

  it("marks each turn by the session that sent it", () => {
    const { result } = parentWorkOf(
      view(link("session-parent"), [
        ["req-child", link("session-parent")],
        ["req-child-2", link("session-other")],
      ]),
    );
    expect(result.current.hasSenders).toBe(true);
    expect(result.current.sentBy("req-child")?.sessionId).toBe("session-parent");
    expect(result.current.sentBy("req-child-2")?.summary?.title).toBe("Reviewer");
    /* the person's own turn, and a turn with no request, have no sender */
    expect(result.current.sentBy("req-mine")).toBeNull();
    expect(result.current.sentBy(null)).toBeNull();
  });

  it("does not guess a parent when the session was not started by another", () => {
    const { result } = parentWorkOf(
      view(null, [["req-child", link("session-parent")]]),
    );
    expect(result.current.parent).toBeNull();
    expect(result.current.sentBy("req-child")?.sessionId).toBe("session-parent");
  });

  it("matches a sender's summary by its full scope", () => {
    const { result } = parentWorkOf(
      view(link("session-parent", "did:key:someone-else")),
    );
    expect(result.current.parent).toEqual({
      sessionId: "session-parent",
      summary: null,
      behaviorName: null,
    });
  });

  it("has nothing to say without provenance", () => {
    const { result } = parentWorkOf(null);
    expect(result.current.parent).toBeNull();
    expect(result.current.hasSenders).toBe(false);
  });
});
