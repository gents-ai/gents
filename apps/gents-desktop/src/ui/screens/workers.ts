/* What the parent knows about the work it delegated: the lineage tree for
   its request joined to the session list and the operations snapshot. Read
   only; every fact here has a field in the 7.8 contract, and where the
   contract is silent the state says so instead of guessing. */
import { useEffect, useMemo, useState } from "react";
import type {
  BackgroundedToolView,
  DesktopOperationsSnapshot,
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
  /* the runtime lists the session but this desktop has no transcript for it */
  unreplicated: boolean;
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

export function useWorkers(shell: Shell): Workers {
  const session = shell.selectedSession;
  const rootRequestId = session?.latestRequestId ?? null;
  const agentDid = shell.selectedDeployment?.agentDid ?? null;
  const sessions = shell.selectedDeployment?.sessions;
  const [tree, setTree] = useState<SubagentTreeView | null>(null);
  const [ops, setOps] = useState<DesktopOperationsSnapshot | null>(null);
  /* the transcript's tool states change as children finish; that is the
     cue to ask again, along with the session list the summaries live in */
  const cue = session?.timelineItems
    .flatMap((i) => (i.kind === "toolGroup" ? i.tools.map((t) => t.statusKind) : []))
    .join();
  /* only a transcript with worker rows needs the lineage and the operations
     snapshot; a plain session never asks the bridge for them */
  const hasWorkers =
    session?.timelineItems.some(
      (i) =>
        i.kind === "toolGroup" &&
        i.tools.some(
          (t) =>
            t.presentation.kind === "subagent" ||
            (t.presentation.kind === "process" && t.awaitMode === "background"),
        ),
    ) ?? false;
  useEffect(() => {
    if (!hasWorkers || !rootRequestId || !agentDid) {
      setTree(null);
      setOps(null);
      return;
    }
    let live = true;
    void shell.api.listSubagentTree({ rootRequestId }).then(
      (t) => live && setTree(t),
      () => live && setTree(null),
    );
    void shell.api.fetchOperationsSnapshot({ agentDid }).then(
      (o) => live && setOps(o),
      () => live && setOps(null),
    );
    return () => {
      live = false;
    };
  }, [shell.api, hasWorkers, rootRequestId, agentDid, cue, sessions]);
  return useMemo(() => {
    if (!tree && !ops) return EMPTY;
    const summaries = new Map((sessions ?? []).map((s) => [s.sessionId, s]));
    const nodes = new Map((tree?.nodes ?? []).map((n) => [n.requestId, n]));
    const edges = new Map((tree?.edges ?? []).map((e) => [e.childRequestId, e]));
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
          /* the runtime resolved the request by gossip rather than from
             documents held here, or the summary has no counted messages:
             the transcript has not replicated to this desktop */
          unreplicated:
            (node != null &&
              node.resolvedVia != null &&
              node.resolvedVia !== "canonical") ||
            (summary != null && summary.messageCount == null),
        };
      },
      byToolCall: (tool) =>
        backgrounded.find(
          (b) =>
            b.toolCallId === tool.itemKey ||
            (tool.childRequestId != null && b.childRequestId === tool.childRequestId),
        ) ?? null,
    };
  }, [tree, ops, sessions]);
}
