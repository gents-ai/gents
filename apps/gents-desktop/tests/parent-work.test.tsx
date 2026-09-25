import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  DesktopListSubagentTreeRequest,
  SessionSummary,
  SubagentTreeView,
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

/* The parent has moved on: its latest request is a newer one, not the
   request that spawned the child, and it has completed. */
const parent = session({
  sessionId: "session-parent",
  latestRequestId: "req-parent-later",
  latestRequestDocId: "bae-parent-later-doc",
  turnState: "completed",
});
const child = session({
  sessionId: "session-child",
  latestRequestId: "req-child",
  provenance: { parent_request_doc_id: PARENT_DOC } as SessionSummary["provenance"],
});

const tree: SubagentTreeView = {
  rootRequestId: "req-parent",
  nodes: [
    {
      requestId: "req-parent",
      resolvedVia: null,
      sessionId: "session-parent",
      agentDid: AGENT,
      behaviorId: "default",
      lifecycleState: "completed",
      subagentDepth: 0,
      causedByParentRequestId: null,
      causedByParentToolCallId: null,
      backendId: null,
    },
    {
      requestId: "req-child",
      resolvedVia: null,
      sessionId: "session-child",
      agentDid: AGENT,
      behaviorId: "crew-explorer",
      lifecycleState: "completed",
      subagentDepth: 1,
      causedByParentRequestId: "req-parent",
      causedByParentToolCallId: "spawn-1",
      backendId: null,
    },
  ],
  edges: [
    {
      parentRequestId: "req-parent",
      childRequestId: "req-child",
      parentToolCallId: "spawn-1",
      toolName: "spawn_subagent",
      awaitMode: "background",
      cancelPolicy: "detach",
      lifecycleState: "completed",
    },
  ],
  truncated: false,
  partialErrors: [],
} as unknown as SubagentTreeView;

function shellFor(listSubagentTree: (r: DesktopListSubagentTreeRequest) => unknown) {
  return {
    selectedSessionId: "session-child",
    selectedDeployment: {
      agentDid: AGENT,
      sessions: [parent, child],
      behaviors: [],
      behaviorConfigs: [],
    },
    api: {
      listSubagentTree: vi.fn(listSubagentTree),
      fetchSessionSnapshot: vi.fn(async () => ({ timelineItems: [] })),
    },
  } as unknown as Shell;
}

describe("a child session's parent work", () => {
  it("roots the lineage at the provenance document and finds the parent through it", async () => {
    const shell = shellFor(async () => tree);
    const { result } = renderHook(() => useParentWork(shell));

    await waitFor(() =>
      expect(result.current.parent?.sessionId).toBe("session-parent"),
    );
    expect(shell.api.listSubagentTree).toHaveBeenCalledWith({
      rootRequestDocId: PARENT_DOC,
      agentDid: AGENT,
      includeTerminal: true,
    });
    expect(result.current.node?.requestId).toBe("req-child");
    expect(result.current.edge?.lifecycleState).toBe("completed");
    expect(shell.api.fetchSessionSnapshot).toHaveBeenCalledWith(
      "session-parent",
      AGENT,
      null,
      undefined,
    );
  });

  it("does not guess a parent when the lineage cannot resolve the document", async () => {
    const shell = shellFor(async () => ({
      ...tree,
      rootRequestId: "",
      nodes: [],
      edges: [],
    }));
    const { result } = renderHook(() => useParentWork(shell));

    await waitFor(() => expect(shell.api.listSubagentTree).toHaveBeenCalled());
    expect(result.current.parent).toBeNull();
    expect(result.current.node).toBeNull();
  });
});
