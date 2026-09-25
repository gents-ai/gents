import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  DesktopListSubagentTreeRequest,
  RenderedTimelineItem,
  RenderedToolCallView,
  SubagentEdgeView,
  SubagentNodeView,
  SubagentTreeView,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";

import { MAX_LINEAGE_ROOTS, lineageRoots, useWorkers } from "../src/ui/screens/workers";
import { workerNow } from "../src/ui/screens/WorkerStep";

const AGENT = "did:key:parent";
const TERMINAL = new Set(["completed", "failed", "cancelled", "dead"]);

const spawn = (
  requestId: string | null,
  childRequestId: string,
  statusKind = "success",
): RenderedToolCallView =>
  ({
    itemKey: `spawn-${childRequestId}`,
    toolName: "spawn_subagent",
    statusKind,
    requestId,
    childRequestId,
    awaitMode: "background",
    presentation: {
      kind: "subagent",
      action: "spawn",
      name: "crew-explorer",
      childRequestId,
      description: "explore",
      output: null,
    },
  }) as unknown as RenderedToolCallView;

const group = (...tools: RenderedToolCallView[]): RenderedTimelineItem =>
  ({
    kind: "toolGroup",
    itemKey: `group-${tools[0]?.itemKey}`,
    messageSequence: null,
    tools,
  }) as RenderedTimelineItem;

const node = (requestId: string, lifecycleState: string): SubagentNodeView => ({
  requestId,
  resolvedVia: null,
  sessionId: `session-${requestId}`,
  agentDid: AGENT,
  behaviorId: "crew-explorer",
  lifecycleState,
  subagentDepth: 1,
  causedByParentRequestId: null,
  causedByParentToolCallId: null,
  backendId: null,
});

const edge = (
  parentRequestId: string,
  childRequestId: string,
  lifecycleState: string,
): SubagentEdgeView => ({
  parentRequestId,
  childRequestId,
  parentToolCallId: `spawn-${childRequestId}`,
  toolName: "spawn_subagent",
  awaitMode: "background",
  cancelPolicy: "detach",
  lifecycleState,
});

/* the runtime's lineage: each root's children, with the handler's rule that
   terminal children are left out unless the caller asks for them */
function lineage(children: Record<string, [string, string][]>) {
  return vi.fn(
    async (request: DesktopListSubagentTreeRequest): Promise<SubagentTreeView> => {
      const root = request.rootRequestId;
      const kept = (children[root] ?? []).filter(
        ([, state]) => request.includeTerminal || !TERMINAL.has(state),
      );
      return {
        rootRequestId: root,
        nodes: [node(root, "processing"), ...kept.map(([c, s]) => node(c, s))],
        edges: kept.map(([c, s]) => edge(root, c, s)),
        truncated: false,
        partialErrors: [],
      };
    },
  );
}

function shellFor(
  api: Shell["api"],
  latestRequestId: string,
  timelineItems: RenderedTimelineItem[],
): Shell {
  return {
    api,
    selectedSession: { sessionId: "parent-session", latestRequestId, timelineItems },
    selectedDeployment: { agentDid: AGENT, sessions: [] },
  } as unknown as Shell;
}

function apiWith(listSubagentTree: ReturnType<typeof lineage>): Shell["api"] {
  return {
    listSubagentTree,
    fetchOperationsSnapshot: vi.fn().mockResolvedValue({ backgroundedTools: [] }),
  } as unknown as Shell["api"];
}

describe("transcript worker lineage", () => {
  it("keeps a finished child's node, edge, await mode, behavior and terminal state", async () => {
    const tree = lineage({
      "req-1": [
        ["child-done", "completed"],
        ["child-live", "processing"],
      ],
    });
    const items = [
      group(spawn("req-1", "child-done"), spawn("req-1", "child-live", "running")),
    ];
    const shell = shellFor(apiWith(tree), "req-1", items);
    const { result } = renderHook(() => useWorkers(shell));

    await waitFor(() =>
      expect(result.current.byChildRequest("child-done")?.node).toBeTruthy(),
    );
    const done = result.current.byChildRequest("child-done")!;
    expect(done.node?.lifecycleState).toBe("completed");
    expect(done.node?.behaviorId).toBe("crew-explorer");
    expect(done.sessionId).toBe("session-child-done");
    expect(done.edge).toMatchObject({
      parentRequestId: "req-1",
      awaitMode: "background",
      lifecycleState: "completed",
    });
    expect(workerNow(spawn("req-1", "child-done"), done)).toEqual({
      tone: "done",
      text: "finished",
    });
    expect(result.current.byChildRequest("child-live")?.edge?.lifecycleState).toBe(
      "processing",
    );
  });

  it("asks the lineage for terminal nodes", async () => {
    const tree = lineage({ "req-1": [["child-done", "completed"]] });
    const shell = shellFor(apiWith(tree), "req-1", [
      group(spawn("req-1", "child-done")),
    ]);
    renderHook(() => useWorkers(shell));
    await waitFor(() => expect(tree).toHaveBeenCalled());
    for (const [request] of tree.mock.calls) {
      expect(request).toMatchObject({ includeTerminal: true, agentDid: AGENT });
    }
  });

  it("keeps workers from an earlier request after a new request becomes latest", async () => {
    const tree = lineage({
      "req-1": [["child-early", "completed"]],
      "req-2": [["child-late", "processing"]],
    });
    const api = apiWith(tree);
    const first = [group(spawn("req-1", "child-early"))];
    const { result, rerender } = renderHook(
      ({ latest, items }: { latest: string; items: RenderedTimelineItem[] }) =>
        useWorkers(shellFor(api, latest, items)),
      { initialProps: { latest: "req-1", items: first } },
    );
    await waitFor(() =>
      expect(result.current.byChildRequest("child-early")?.node).toBeTruthy(),
    );

    rerender({
      latest: "req-2",
      items: [...first, group(spawn("req-2", "child-late", "running"))],
    });
    await waitFor(() =>
      expect(result.current.byChildRequest("child-late")?.node).toBeTruthy(),
    );

    const early = result.current.byChildRequest("child-early");
    expect(early?.node?.lifecycleState).toBe("completed");
    expect(early?.edge?.parentRequestId).toBe("req-1");
    /* the earlier root's rows did not change, so it was not asked again */
    const roots = tree.mock.calls.map(([r]) => r.rootRequestId);
    expect(roots.filter((r) => r === "req-1")).toHaveLength(1);
    expect(roots.filter((r) => r === "req-2")).toHaveLength(1);
  });

  it("asks a root again only when its own rows change", async () => {
    const tree = lineage({
      "req-1": [["child-a", "processing"]],
      "req-2": [["child-b", "processing"]],
    });
    const api = apiWith(tree);
    const a = (status: string) => group(spawn("req-1", "child-a", status));
    const b = group(spawn("req-2", "child-b", "running"));
    const { rerender } = renderHook(
      ({ items }: { items: RenderedTimelineItem[] }) =>
        useWorkers(shellFor(api, "req-2", items)),
      { initialProps: { items: [a("running"), b] } },
    );
    await waitFor(() => expect(tree).toHaveBeenCalledTimes(2));
    rerender({ items: [a("running"), b] });
    rerender({ items: [a("success"), b] });
    await waitFor(() => expect(tree).toHaveBeenCalledTimes(3));
    expect(tree.mock.calls[2]![0].rootRequestId).toBe("req-1");
  });
});

describe("lineage roots", () => {
  it("names each worker-owning request once, in order of its latest row", () => {
    const roots = lineageRoots(
      [group(spawn("req-1", "a"), spawn("req-2", "b")), group(spawn("req-1", "c"))],
      "req-9",
    );
    expect([...roots.keys()]).toEqual(["req-2", "req-1"]);
  });

  it("attributes a row that names no request to the latest request", () => {
    expect([...lineageRoots([group(spawn(null, "a"))], "req-9").keys()]).toEqual([
      "req-9",
    ]);
  });

  it("bounds the roots to the most recent requests", () => {
    const items = Array.from({ length: MAX_LINEAGE_ROOTS + 4 }, (_, i) =>
      group(spawn(`req-${i}`, `child-${i}`)),
    );
    const roots = [...lineageRoots(items, null).keys()];
    expect(roots).toHaveLength(MAX_LINEAGE_ROOTS);
    expect(roots[0]).toBe("req-4");
    expect(roots.at(-1)).toBe(`req-${MAX_LINEAGE_ROOTS + 3}`);
  });
});
