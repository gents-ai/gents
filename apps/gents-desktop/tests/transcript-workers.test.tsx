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

import {
  LINEAGE_REFRESH_MS,
  MAX_LINEAGE_ROOTS,
  lineageRoots,
  useWorkers,
} from "../src/ui/screens/workers";
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
function lineage(
  children: Record<string, [string, string][]>,
  rootState = "processing",
) {
  return vi.fn(
    async (request: DesktopListSubagentTreeRequest): Promise<SubagentTreeView> => {
      const root = request.rootRequestId;
      const kept = (children[root] ?? []).filter(
        ([, state]) => request.includeTerminal || !TERMINAL.has(state),
      );
      return {
        rootRequestId: root,
        nodes: [node(root, rootState), ...kept.map(([c, s]) => node(c, s))],
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
  { sessionId = "parent-session", sessions = [] as unknown[] } = {},
): Shell {
  return {
    api,
    selectedSession: { sessionId, latestRequestId, timelineItems },
    selectedDeployment: { agentDid: AGENT, sessions },
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

  it("links no session to a child the lineage does not know, even when a summary's latest request is the child", async () => {
    const tree = lineage({ "req-1": [] });
    const items = [group(spawn("req-1", "child-unknown", "running"))];
    const shell = shellFor(apiWith(tree), "req-1", items, {
      sessions: [
        {
          sessionId: "unrelated-session",
          latestRequestId: "child-unknown",
        },
      ],
    });
    const { result } = renderHook(() => useWorkers(shell));

    await waitFor(() => expect(tree).toHaveBeenCalled());
    await waitFor(() => expect(result.current.loaded).toBe(true));
    expect(result.current.byChildRequest("child-unknown")).toBeNull();
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
    const tree = lineage(
      {
        "req-1": [["child-early", "completed"]],
        "req-2": [["child-late", "processing"]],
      },
      "completed",
    );
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
    /* the earlier root's rows did not change and its tree is settled, so it
       was not asked again */
    const roots = tree.mock.calls.map(([r]) => r.rootRequestId);
    expect(roots.filter((r) => r === "req-1")).toHaveLength(1);
    expect(roots.filter((r) => r === "req-2")).toHaveLength(1);
  });

  it("asks a root with a settled tree again only when its own rows change", async () => {
    const tree = lineage(
      {
        "req-1": [["child-a", "completed"]],
        "req-2": [["child-b", "completed"]],
      },
      "completed",
    );
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

describe("worker lineage freshness", () => {
  it("does not keep another session's tree for the same root and rows", async () => {
    const tree = lineage({ "req-1": [["child-a", "completed"]] }, "completed");
    const api = apiWith(tree);
    const items = [group(spawn("req-1", "child-a"))];
    const { result, rerender } = renderHook(
      ({ sessionId }: { sessionId: string }) =>
        useWorkers(shellFor(api, "req-1", items, { sessionId })),
      { initialProps: { sessionId: "session-a" } },
    );
    await waitFor(() =>
      expect(result.current.byChildRequest("child-a")?.node).toBeTruthy(),
    );
    tree.mockImplementationOnce(async () => ({
      rootRequestId: "req-1",
      nodes: [node("req-1", "completed"), node("child-b", "completed")],
      edges: [edge("req-1", "child-b", "completed")],
      truncated: false,
      partialErrors: [],
    }));
    rerender({ sessionId: "session-b" });
    /* not even for the render that switches sessions */
    expect(result.current.byChildRequest("child-a")?.node ?? null).toBeNull();
    await waitFor(() =>
      expect(result.current.byChildRequest("child-b")?.node).toBeTruthy(),
    );
    expect(tree).toHaveBeenCalledTimes(2);
    expect(result.current.byChildRequest("child-a")?.node ?? null).toBeNull();
  });

  it("refreshes a child that finishes after its spawn row settled, and keeps the last tree on a failed ask", async () => {
    const children: Record<string, [string, string][]> = {
      "req-1": [["child-late", "processing"]],
    };
    const tree = lineage(children, "completed");
    const api = apiWith(tree);
    const items = [group(spawn("req-1", "child-late", "success"))];
    const { result, rerender } = renderHook(
      ({ sessions }: { sessions: unknown[] }) =>
        useWorkers(shellFor(api, "req-1", items, { sessions })),
      { initialProps: { sessions: [] as unknown[] } },
    );
    await waitFor(() =>
      expect(result.current.byChildRequest("child-late")?.node?.lifecycleState).toBe(
        "processing",
      ),
    );

    tree.mockRejectedValueOnce(new Error("peer unreachable"));
    rerender({ sessions: [{ sessionId: "other", turnState: "running" }] });
    await waitFor(() => expect(tree).toHaveBeenCalledTimes(2));
    expect(result.current.byChildRequest("child-late")?.node?.lifecycleState).toBe(
      "processing",
    );

    children["req-1"] = [["child-late", "completed"]];
    rerender({ sessions: [{ sessionId: "other", turnState: "completed" }] });
    await waitFor(() =>
      expect(result.current.byChildRequest("child-late")?.node?.lifecycleState).toBe(
        "completed",
      ),
    );

    rerender({ sessions: [{ sessionId: "other", turnState: "idle" }] });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(tree).toHaveBeenCalledTimes(3);
  });
});

describe("worker lineage settling", () => {
  it("treats a tree with partial errors as unsettled and asks again on the next cue", async () => {
    const tree = lineage({ "req-1": [["child-a", "completed"]] }, "completed");
    const partial = async (request: DesktopListSubagentTreeRequest) => ({
      ...(await tree.getMockImplementation()!(request)),
      partialErrors: ["peer-b: unreachable"],
    });
    tree.mockImplementationOnce(partial);
    const api = apiWith(tree);
    const items = [group(spawn("req-1", "child-a"))];
    const { rerender } = renderHook(
      ({ sessions }: { sessions: unknown[] }) =>
        useWorkers(shellFor(api, "req-1", items, { sessions })),
      { initialProps: { sessions: [] as unknown[] } },
    );
    await waitFor(() => expect(tree).toHaveBeenCalledTimes(1));
    rerender({ sessions: [{ sessionId: "other", turnState: "running" }] });
    await waitFor(() => expect(tree).toHaveBeenCalledTimes(2));
    rerender({ sessions: [{ sessionId: "other", turnState: "idle" }] });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(tree).toHaveBeenCalledTimes(2);
  });

  it("asks again on the next cue after a failed refresh of a settled tree", async () => {
    const tree = lineage({ "req-1": [["child-a", "completed"]] }, "completed");
    const api = apiWith(tree);
    const { result, rerender } = renderHook(
      ({ status, sessions }: { status: string; sessions: unknown[] }) =>
        useWorkers(
          shellFor(api, "req-1", [group(spawn("req-1", "child-a", status))], {
            sessions,
          }),
        ),
      { initialProps: { status: "running", sessions: [] as unknown[] } },
    );
    await waitFor(() =>
      expect(result.current.byChildRequest("child-a")?.node).toBeTruthy(),
    );
    tree.mockRejectedValueOnce(new Error("bridge busy"));
    rerender({ status: "success", sessions: [] });
    await waitFor(() => expect(tree).toHaveBeenCalledTimes(2));
    expect(result.current.byChildRequest("child-a")?.node).toBeTruthy();
    rerender({ status: "success", sessions: [{ sessionId: "other" }] });
    await waitFor(() => expect(tree).toHaveBeenCalledTimes(3));
  });

  it("polls an unsettled tree with no local cue, and stops once it settles", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const children: Record<string, [string, string][]> = {
        "req-1": [["child-remote", "processing"]],
      };
      const tree = lineage(children, "completed");
      const shell = shellFor(apiWith(tree), "req-1", [
        group(spawn("req-1", "child-remote", "success")),
      ]);
      const { result } = renderHook(() => useWorkers(shell));
      await waitFor(() => expect(tree).toHaveBeenCalledTimes(1));

      children["req-1"] = [["child-remote", "completed"]];
      await vi.advanceTimersByTimeAsync(LINEAGE_REFRESH_MS + 1_000);
      await waitFor(() =>
        expect(
          result.current.byChildRequest("child-remote")?.node?.lifecycleState,
        ).toBe("completed"),
      );
      const asked = tree.mock.calls.length;
      await vi.advanceTimersByTimeAsync(LINEAGE_REFRESH_MS * 3);
      expect(tree).toHaveBeenCalledTimes(asked);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("lineage roots", () => {
  it("names each subagent-owning request once, in order of its latest row", () => {
    const roots = lineageRoots([
      group(spawn("req-1", "a"), spawn("req-2", "b")),
      group(spawn("req-1", "c")),
    ]);
    expect([...roots.keys()]).toEqual(["req-2", "req-1"]);
  });

  it("gives a row that names no request no lineage root", () => {
    expect([...lineageRoots([group(spawn(null, "a"))]).keys()]).toEqual([]);
  });

  it("does not treat background process rows as lineage roots", () => {
    const process = {
      ...spawn("req-1", "p"),
      toolName: "bash",
      presentation: { kind: "process" },
    } as unknown as RenderedToolCallView;
    expect([...lineageRoots([group(process)]).keys()]).toEqual([]);
  });

  it("bounds the roots to the most recent requests", () => {
    const items = Array.from({ length: MAX_LINEAGE_ROOTS + 4 }, (_, i) =>
      group(spawn(`req-${i}`, `child-${i}`)),
    );
    const roots = [...lineageRoots(items).keys()];
    expect(roots).toHaveLength(MAX_LINEAGE_ROOTS);
    expect(roots[0]).toBe("req-4");
    expect(roots.at(-1)).toBe(`req-${MAX_LINEAGE_ROOTS + 3}`);
  });
});
