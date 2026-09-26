import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type {
  CausedRequestView,
  SessionProvenanceView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";

import { useParentWork } from "../src/ui/screens/parentWork";

const AGENT = "did:key:parent";
const PARENT_DOC = "bae-parent-request-doc";

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
const child = session({
  sessionId: "session-child",
  provenance: { parent_request_doc_id: PARENT_DOC } as SessionSummary["provenance"],
});

const received = (
  requestId: string,
  causedByRequestDocId: string,
  causedBySessionId: string | null,
  createdAt: string,
): CausedRequestView => ({
  requestId,
  requestDocId: `doc-${requestId}`,
  sessionId: "session-child",
  agentDid: AGENT,
  behaviorId: "crew-explorer",
  lifecycleState: "completed",
  interruptRequestedAt: null,
  createdAt,
  hop: 1,
  causedByRequestId: "req-parent",
  causedByRequestDocId,
  causedByToolCallId: "call-1",
  causedBySessionId,
});

const view = (rows: CausedRequestView[], sessionId = "session-child") =>
  ({ sessionId, received: rows, sent: [], truncated: false }) as SessionProvenanceView;

function shellFor(sessions: SessionSummary[] = [parent, other, child]) {
  return {
    selectedSessionId: "session-child",
    selectedDeployment: {
      agentDid: AGENT,
      sessions,
      behaviors: [],
      behaviorConfigs: [],
    },
  } as unknown as Shell;
}

describe("the sessions that sent work into this one", () => {
  it("names the session whose call started it, through its provenance document", () => {
    const { result } = renderHook(() =>
      useParentWork(
        shellFor(),
        view([
          received("req-child-2", "doc-other", "session-other", "2"),
          received("req-child", PARENT_DOC, "session-parent", "1"),
        ]),
      ),
    );
    expect(result.current.parent?.sessionId).toBe("session-parent");
    expect(result.current.parent?.summary?.title).toBe("Lead");
    expect(result.current.hasSenders).toBe(true);
  });

  it("marks each turn by the session whose call caused its request", () => {
    const { result } = renderHook(() =>
      useParentWork(
        shellFor(),
        view([
          received("req-child", PARENT_DOC, "session-parent", "1"),
          received("req-child-2", "doc-other", "session-other", "2"),
        ]),
      ),
    );
    expect(result.current.sentBy("req-child")?.sessionId).toBe("session-parent");
    expect(result.current.sentBy("req-child-2")?.summary?.title).toBe("Reviewer");
    /* the person's own turn, and a turn with no request, have no sender */
    expect(result.current.sentBy("req-mine")).toBeNull();
    expect(result.current.sentBy(null)).toBeNull();
  });

  it("does not guess a parent when the session was not started by another", () => {
    const root = session({ sessionId: "session-child" });
    const { result } = renderHook(() =>
      useParentWork(
        shellFor([parent, root]),
        view([received("req-child", "doc-x", "session-parent", "1")]),
      ),
    );
    expect(result.current.parent).toBeNull();
    expect(result.current.sentBy("req-child")?.sessionId).toBe("session-parent");
  });

  it("keeps a sender it cannot list as a bare session, without a title", () => {
    const { result } = renderHook(() =>
      useParentWork(
        shellFor([child]),
        view([received("req-child", PARENT_DOC, "session-elsewhere", "1")]),
      ),
    );
    expect(result.current.parent).toEqual({
      sessionId: "session-elsewhere",
      summary: null,
      behaviorName: null,
    });
  });

  it("has nothing to say without provenance", () => {
    const { result } = renderHook(() => useParentWork(shellFor(), null));
    expect(result.current.parent).toBeNull();
    expect(result.current.hasSenders).toBe(false);
  });
});
