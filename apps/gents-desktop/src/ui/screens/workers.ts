/* What the parent knows about the work it delegated: the lineage trees for
   the requests in its transcript that spawned workers, joined to the session
   list and the operations snapshot. Read only; every fact here has a field in
   the bridge contract, and where the contract is silent the state says so
   instead of guessing. */
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  BackgroundedToolView,
  DesktopOperationsSnapshot,
  RenderedTimelineItem,
  RenderedToolCallView,
  SessionSummary,
  SubagentEdgeView,
  SubagentNodeView,
  SubagentTreeView,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";

export type WorkerState = {
  /* the child session, when the runtime told us which one */
  sessionId: string | null;
  summary: SessionSummary | null;
  node: SubagentNodeView | null;
  edge: SubagentEdgeView | null;
  /* the parent's tool call for this child, if this desktop has it */
  background: BackgroundedToolView | null;
};

export type Workers = {
  byChildRequest: (childRequestId: string) => WorkerState | null;
  byToolCall: (tool: RenderedToolCallView) => BackgroundedToolView | null;
  loaded: boolean;
};

const EMPTY: Workers = {
  byChildRequest: () => null,
  byToolCall: () => null,
  loaded: false,
};

/* A transcript names at most this many lineage roots at once; the most
   recent win. Each is one bridge call, so the bound is the call bound. */
export const MAX_LINEAGE_ROOTS = 16;

const isWorker = (t: RenderedToolCallView) =>
  t.presentation.kind === "subagent" ||
  (t.presentation.kind === "process" && t.awaitMode === "background");

/* The requests in this transcript that own worker rows, each with the
   statuses of its rows: a root is asked again only when its own rows change.
   A row that does not name its request (an older bridge) is attributed to
   the session's latest request, as the transcript did before rows named it. */
export function lineageRoots(
  items: readonly RenderedTimelineItem[] | undefined,
  latestRequestId: string | null,
): Map<string, string> {
  const statuses = new Map<string, string[]>();
  for (const item of items ?? []) {
    if (item.kind !== "toolGroup") continue;
    for (const tool of item.tools) {
      if (!isWorker(tool)) continue;
      const root = tool.requestId ?? latestRequestId;
      if (!root) continue;
      const seen = statuses.get(root);
      /* re-inserting keeps the map in order of each root's latest row */
      statuses.delete(root);
      statuses.set(root, [...(seen ?? []), `${tool.itemKey}:${tool.statusKind}`]);
    }
  }
  const recent = [...statuses].slice(-MAX_LINEAGE_ROOTS);
  return new Map(recent.map(([root, s]) => [root, s.join()]));
}

export function useWorkers(shell: Shell): Workers {
  const session = shell.selectedSession;
  const latestRequestId = session?.latestRequestId ?? null;
  const agentDid = shell.selectedDeployment?.agentDid ?? null;
  const sessions = shell.selectedDeployment?.sessions;
  const [trees, setTrees] = useState<ReadonlyMap<string, SubagentTreeView>>(
    () => new Map(),
  );
  const [ops, setOps] = useState<DesktopOperationsSnapshot | null>(null);
  /* the transcript's tool states change as children finish; that is the
     cue to ask for the operations snapshot again, along with the session
     list the summaries live in */
  const cue = session?.timelineItems
    .flatMap((i) => (i.kind === "toolGroup" ? i.tools.map((t) => t.statusKind) : []))
    .join();
  const roots = useMemo(
    () => lineageRoots(session?.timelineItems, latestRequestId),
    [session?.timelineItems, latestRequestId],
  );
  /* only a transcript with worker rows needs the lineage and the operations
     snapshot; a plain session never asks the bridge for them */
  const hasWorkers =
    session?.timelineItems.some(
      (i) => i.kind === "toolGroup" && i.tools.some(isWorker),
    ) ?? false;
  const rootsKey = JSON.stringify([...roots]);
  /* per root, the row statuses its current tree was asked for; a root whose
     rows are unchanged keeps its tree */
  const asked = useRef(new Map<string, string>());
  const askedFor = useRef(agentDid);
  useEffect(() => {
    if (askedFor.current !== agentDid) {
      askedFor.current = agentDid;
      asked.current = new Map();
      setTrees(new Map());
    }
    if (!agentDid) return;
    const wanted = new Map(JSON.parse(rootsKey) as [string, string][]);
    const current = (root: string, statuses: string) =>
      askedFor.current === agentDid && asked.current.get(root) === statuses;
    for (const root of [...asked.current.keys()]) {
      if (!wanted.has(root)) asked.current.delete(root);
    }
    setTrees((held) => {
      const kept = [...held].filter(([root]) => wanted.has(root));
      return kept.length === held.size ? held : new Map(kept);
    });
    for (const [root, statuses] of wanted) {
      if (asked.current.get(root) === statuses) continue;
      asked.current.set(root, statuses);
      /* the transcript is history: finished workers stay in the lineage */
      void shell.api
        .listSubagentTree({ rootRequestId: root, agentDid, includeTerminal: true })
        .then(
          (tree) => {
            if (current(root, statuses))
              setTrees((held) => new Map(held).set(root, tree));
          },
          () => {
            if (!current(root, statuses)) return;
            /* asked again on the next change to this root's rows */
            asked.current.delete(root);
            setTrees((held) => {
              if (!held.has(root)) return held;
              const next = new Map(held);
              next.delete(root);
              return next;
            });
          },
        );
    }
  }, [shell.api, agentDid, rootsKey]);
  useEffect(() => {
    if (!hasWorkers || !agentDid) {
      setOps(null);
      return;
    }
    let live = true;
    void shell.api.fetchOperationsSnapshot({ agentDid }).then(
      (o) => live && setOps(o),
      () => live && setOps(null),
    );
    return () => {
      live = false;
    };
  }, [shell.api, hasWorkers, agentDid, cue, sessions]);
  return useMemo(() => {
    if (trees.size === 0 && !ops) return EMPTY;
    const summaries = new Map((sessions ?? []).map((s) => [s.sessionId, s]));
    const nodes = new Map<string, SubagentNodeView>();
    const edges = new Map<string, SubagentEdgeView>();
    for (const tree of trees.values()) {
      for (const n of tree.nodes) nodes.set(n.requestId, n);
      for (const e of tree.edges) edges.set(e.childRequestId, e);
    }
    const backgrounded = ops?.backgroundedTools ?? [];
    return {
      loaded: true,
      byChildRequest: (childRequestId) => {
        const node = nodes.get(childRequestId) ?? null;
        const edge = edges.get(childRequestId) ?? null;
        /* without lineage, a summary whose latest request is the child is
           the next best link */
        const sessionId =
          node?.sessionId ??
          (sessions ?? []).find((s) => s.latestRequestId === childRequestId)
            ?.sessionId ??
          null;
        if (!node && !edge && !sessionId) return null;
        const summary = sessionId ? (summaries.get(sessionId) ?? null) : null;
        return {
          sessionId,
          summary,
          node,
          edge,
          background:
            backgrounded.find((b) => b.childRequestId === childRequestId) ?? null,
        };
      },
      byToolCall: (tool) =>
        backgrounded.find(
          (b) =>
            b.toolCallId === tool.itemKey ||
            (tool.childRequestId != null && b.childRequestId === tool.childRequestId),
        ) ?? null,
    };
  }, [trees, ops, sessions]);
}
