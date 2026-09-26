/* What a child session knows about the work it was given: the parent that
   started it, the lineage edge that binds them, and the instructions the
   parent sent since. Read from provenance, the lineage tree and the
   parent's own transcript; where the contract has no link (a steer lands
   in the child as a plain user turn) the match is by the instruction's
   text, and the notes say so. */
import { useEffect, useMemo, useState } from "react";
import type {
  DesktopSessionSnapshot,
  SessionSummary,
  SubagentEdgeView,
  SubagentNodeView,
  SubagentTreeView,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { behaviorName } from "./behavior";

export type ParentInstruction = {
  text: string;
  interrupt: boolean;
  receipt: string | null;
  at: string | null;
};

export type ParentWork = {
  parent: SessionSummary | null;
  /* this session's own summary, as the runtime lists it */
  summary: SessionSummary | null;
  node: SubagentNodeView | null;
  edge: SubagentEdgeView | null;
  assignment: string | null;
  instructions: ParentInstruction[];
  /* the parent's behavior, for the sender mark on turns it sent */
  parentBehaviorName: string | null;
  /* what a user turn is, if the parent sent it; null when it is the person's own */
  sentBy: (content: string) => "assignment" | "instruction" | "interruption" | null;
};

const NONE: ParentWork = {
  parent: null,
  summary: null,
  node: null,
  edge: null,
  assignment: null,
  instructions: [],
  parentBehaviorName: null,
  sentBy: () => null,
};

const strip = (s: string | null | undefined) =>
  (s ?? "").replace(/^\[interrupt\]\s*/, "").trim();

export function useParentWork(shell: Shell): ParentWork {
  const sessionId = shell.selectedSessionId;
  const deployment = shell.selectedDeployment;
  const sessions = deployment?.sessions;
  const agentDid = deployment?.agentDid ?? null;
  const summary = useMemo(
    () => sessions?.find((s) => s.sessionId === sessionId) ?? null,
    [sessions, sessionId],
  );
  /* provenance names the exact request document that spawned this session;
     the lineage owner roots the tree at that document, and the parent is the
     session its root node belongs to */
  const parentRequestDocId = summary?.provenance?.parent_request_doc_id ?? null;
  const [tree, setTree] = useState<SubagentTreeView | null>(null);
  const root = tree?.nodes.find((n) => n.requestId === tree.rootRequestId) ?? null;
  const parentSessionId = root?.sessionId ?? null;
  const parent = useMemo(
    () =>
      parentSessionId
        ? (sessions?.find((s) => s.sessionId === parentSessionId) ?? null)
        : null,
    [sessions, parentSessionId],
  );
  const [parentSnapshot, setParentSnapshot] = useState<DesktopSessionSnapshot | null>(
    null,
  );
  useEffect(() => {
    if (!parentRequestDocId || !agentDid) {
      setTree(null);
      return;
    }
    let live = true;
    void shell.api
      .listSubagentTree({
        rootRequestDocId: parentRequestDocId,
        agentDid,
        includeTerminal: true,
      })
      .then(
        (t) => live && setTree(t),
        () => live && setTree(null),
      );
    return () => {
      live = false;
    };
  }, [shell.api, parentRequestDocId, agentDid, summary?.updatedAt]);
  useEffect(() => {
    if (!parent || !agentDid) {
      setParentSnapshot(null);
      return;
    }
    let live = true;
    void shell.api
      .fetchSessionSnapshot(parent.sessionId, agentDid, null, undefined)
      .then(
        (s) => live && setParentSnapshot(s),
        () => live && setParentSnapshot(null),
      );
    return () => {
      live = false;
    };
  }, [shell.api, parent, agentDid, parent?.updatedAt]);
  return useMemo(() => {
    if (!summary || !parentRequestDocId) return NONE;
    const node = tree?.nodes.find((n) => n.sessionId === sessionId) ?? null;
    const edge = node
      ? (tree?.edges.find((e) => e.childRequestId === node.requestId) ?? null)
      : null;
    const tools =
      parentSnapshot?.timelineItems.flatMap((i) =>
        i.kind === "toolGroup" ? i.tools : [],
      ) ?? [];
    const mine = tools.filter(
      (t) =>
        t.presentation.kind === "subagent" &&
        node != null &&
        (t.childRequestId === node.requestId ||
          t.presentation.childRequestId === node.requestId),
    );
    const spawn = mine.find(
      (t) => t.presentation.kind === "subagent" && t.presentation.action === "spawn",
    );
    const assignment =
      spawn?.presentation.kind === "subagent"
        ? strip(spawn.presentation.description) || null
        : null;
    const instructions: ParentInstruction[] = mine
      .filter(
        (t) => t.presentation.kind === "subagent" && t.presentation.action === "steer",
      )
      .map((t) => {
        const p = t.presentation as Extract<
          typeof t.presentation,
          { kind: "subagent" }
        >;
        return {
          text: strip(p.description),
          interrupt: Boolean(p.description?.startsWith("[interrupt]")),
          receipt: p.output?.trim() || null,
          at: t.startedAt ?? null,
        };
      });
    const parentBehaviorName = parent
      ? behaviorName(parent.behaviorId, deployment)
      : null;
    /* the assignment is the first turn; a steer is a later one. Matched by
       text: the contract does not mark a turn as sent by another request */
    const sentBy = (content: string) => {
      const c = content.trim();
      if (assignment && c === assignment) return "assignment" as const;
      const hit = instructions.find((i) => i.text === strip(c));
      if (hit)
        return hit.interrupt ? ("interruption" as const) : ("instruction" as const);
      return null;
    };
    return {
      parent,
      summary,
      node,
      edge,
      assignment,
      instructions,
      parentBehaviorName,
      sentBy,
    };
  }, [
    summary,
    parentRequestDocId,
    tree,
    parentSnapshot,
    sessionId,
    parent,
    deployment,
  ]);
}
