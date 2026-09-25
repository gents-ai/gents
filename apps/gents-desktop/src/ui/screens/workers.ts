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
import { isLive } from "@/lib/live";

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

/* A transcript asks for at most this many lineage roots at once; the most
   recent win. This bounds the bridge calls, not the size of each root's
   graph walk. */
export const MAX_LINEAGE_ROOTS = 16;

const isWorker = (t: RenderedToolCallView) =>
  t.presentation.kind === "subagent" ||
  (t.presentation.kind === "process" && t.awaitMode === "background");

/* The requests in this transcript that own subagent rows, each with the
   statuses of its rows. Only subagent rows have lineage edges, and a row
   that names no request has no lineage root: it is shown without one. */
export function lineageRoots(
  items: readonly RenderedTimelineItem[] | undefined,
): Map<string, string> {
  const statuses = new Map<string, string[]>();
  for (const item of items ?? []) {
    if (item.kind !== "toolGroup") continue;
    for (const tool of item.tools) {
      if (tool.presentation.kind !== "subagent" || !tool.requestId) continue;
      const root = tool.requestId;
      const seen = statuses.get(root);
      /* re-inserting keeps the map in order of each root's latest row */
      statuses.delete(root);
      statuses.set(root, [...(seen ?? []), `${tool.itemKey}:${tool.statusKind}`]);
    }
  }
  const recent = [...statuses].slice(-MAX_LINEAGE_ROOTS);
  return new Map(recent.map(([root, s]) => [root, s.join()]));
}

/* A tree is settled when every node and edge is terminal and every access
   answered. An unsettled tree can change without the parent's rows
   changing: a background spawn row settles when its receipt arrives, long
   before the child finishes, and a child on another deployment moves no
   local session summary. */
export const treeSettled = (tree: SubagentTreeView) =>
  tree.partialErrors.length === 0 &&
  !tree.nodes.some((n) => isLive(n.lifecycleState)) &&
  !tree.edges.some((e) => isLive(e.lifecycleState));

/* While any held tree is unsettled it is asked again at most this often,
   besides on transcript and session-list changes: a remote child's progress
   reaches no local cue. Settled trees are never polled. */
export const LINEAGE_REFRESH_MS = 10_000;

type Held<T> = { scope: string; value: T };

export function useWorkers(shell: Shell): Workers {
  const session = shell.selectedSession;
  const sessionId = session?.sessionId ?? null;
  const agentDid = shell.selectedDeployment?.agentDid ?? null;
  const sessions = shell.selectedDeployment?.sessions;
  /* trees belong to one agent's session, ops to one agent; state held for
     another scope is ignored from the render that changes it */
  const scope = `${agentDid ?? ""}\u0000${sessionId ?? ""}`;
  const [held, setHeld] = useState<Held<ReadonlyMap<string, SubagentTreeView>>>(() => ({
    scope,
    value: new Map(),
  }));
  const [heldOps, setHeldOps] = useState<Held<DesktopOperationsSnapshot> | null>(null);
  const trees = useMemo<ReadonlyMap<string, SubagentTreeView>>(
    () => (held.scope === scope ? held.value : new Map()),
    [held, scope],
  );
  const ops = heldOps?.scope === agentDid ? heldOps.value : null;
  /* the transcript's tool states change as children finish; that is the
     cue to ask for the operations snapshot again, along with the session
     list the summaries live in */
  const cue = session?.timelineItems
    .flatMap((i) => (i.kind === "toolGroup" ? i.tools.map((t) => t.statusKind) : []))
    .join();
  /* the session list changes when a child session on this or another
     deployment moves; its value, not its identity, is the cue */
  const sessionsCue = (sessions ?? [])
    .map((s) => `${s.sessionId}:${s.turnState ?? ""}:${s.updatedAt ?? ""}`)
    .join();
  const roots = useMemo(
    () => lineageRoots(session?.timelineItems),
    [session?.timelineItems],
  );
  const rootsKey = useMemo(
    () => [...roots].map((entry) => entry.join("\u0001")).join("\u0002"),
    [roots],
  );
  const rootsRef = useRef(roots);
  rootsRef.current = roots;
  /* subagent and background process rows both have operations facts; only
     subagent rows have lineage */
  const hasWorkers = useMemo(
    () =>
      session?.timelineItems.some(
        (i) => i.kind === "toolGroup" && i.tools.some(isWorker),
      ) ?? false,
    [session?.timelineItems],
  );
  const unsettled = [...trees.values()].some((t) => !treeSettled(t));
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!unsettled) return;
    const timer = window.setInterval(() => setTick((t) => t + 1), LINEAGE_REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [unsettled]);
  const scoped = useRef(scope);
  /* per root: the row statuses its tree was asked for, and the ask in flight */
  const asked = useRef(new Map<string, string>());
  const pending = useRef(new Map<string, number>());
  const generation = useRef(0);
  const treesRef = useRef(trees);
  treesRef.current = trees;
  useEffect(() => {
    if (scoped.current !== scope) {
      scoped.current = scope;
      asked.current = new Map();
      pending.current = new Map();
    }
    if (!agentDid) return;
    const wanted = rootsRef.current;
    for (const root of [...asked.current.keys()]) {
      if (!wanted.has(root)) {
        asked.current.delete(root);
        pending.current.delete(root);
      }
    }
    const put = (
      update: (
        trees: ReadonlyMap<string, SubagentTreeView>,
      ) => ReadonlyMap<string, SubagentTreeView>,
    ) =>
      setHeld((prev) => {
        const base = prev.scope === scope ? prev.value : new Map();
        const value = update(base);
        return prev.scope === scope && value === prev.value ? prev : { scope, value };
      });
    put((base) => {
      const kept = [...base].filter(([root]) => wanted.has(root));
      return kept.length === base.size ? base : new Map(kept);
    });
    for (const [root, statuses] of wanted) {
      if (pending.current.has(root)) continue;
      const tree = treesRef.current.get(root);
      /* unchanged rows keep a settled tree; an unsettled one is asked again
         on every cue and refresh tick */
      if (asked.current.get(root) === statuses && tree && treeSettled(tree)) continue;
      const ask = ++generation.current;
      asked.current.set(root, statuses);
      pending.current.set(root, ask);
      const settle = () => {
        if (scoped.current !== scope || pending.current.get(root) !== ask) return false;
        pending.current.delete(root);
        return true;
      };
      /* the transcript is history: finished workers stay in the lineage */
      void shell.api
        .listSubagentTree({ rootRequestId: root, agentDid, includeTerminal: true })
        .then(
          (next) => {
            if (settle()) put((base) => new Map(base).set(root, next));
          },
          () => {
            /* the last known tree stays; the next cue asks again */
            if (settle()) asked.current.delete(root);
          },
        );
    }
  }, [shell.api, agentDid, scope, rootsKey, cue, sessionsCue, tick]);
  useEffect(() => {
    if (!hasWorkers || !agentDid) {
      setHeldOps(null);
      return;
    }
    let live = true;
    void shell.api.fetchOperationsSnapshot({ agentDid }).then(
      (o) => live && setHeldOps({ scope: agentDid, value: o }),
      () => live && setHeldOps(null),
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
