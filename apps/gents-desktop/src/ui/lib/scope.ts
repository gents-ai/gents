/* What a list is looking at. One shape for sessions, the mailbox and the
   sidebar's recents, each with its own default: `selected` is the node the
   shell has selected (what every screen does today), `working` the node
   this machine runs, `all` every paired node, or a chosen set. Agents
   narrow within those nodes. */
import type { MailboxItemView, SessionSummary } from "@source-inc/gents-desktop-client";
import type { FleetState, NodeView } from "../../hooks/fleetStore";
import { nodeDidOf, workingNode, type NodeDid, type NodeLike } from "./nodes";

export type NodeScope = "selected" | "working" | "all" | readonly NodeDid[];
export type Scope = { nodes: NodeScope; agents: readonly string[] };

export type ScopedView = "sessions" | "mailbox" | "recents";

/* the defaults the design settles on: the mailbox is what waits on the
   person, wherever it came from; sessions and recents are the node this
   machine runs, or the selection when it runs none. */
export const defaultScope = (view: ScopedView): Scope => ({
  nodes: view === "mailbox" ? "all" : "working",
  agents: [],
});

export type ScopeContext = {
  nodes: readonly NodeLike[];
  selectedNodeDid: NodeDid | null;
  /** the home's agent DID, which marks the working node */
  homeDid: string | null | undefined;
  /** each node's sessions and mailbox items from the fleet store, which keep
      their identity while unchanged */
  fleet: Pick<FleetState, "sessionsOf" | "mailboxOf">;
};

/** The fleet's nodes, in the snapshot's order. */
export const fleetNodes = (fleet: FleetState): NodeView[] =>
  fleet.nodeKeys.flatMap((key) => fleet.nodes[key] ?? []);

/** A scope's context from the fleet store and the selection. */
export const scopeContextOf = (
  fleet: FleetState,
  selectedNodeDid: NodeDid | null,
  homeDid: string | null,
): ScopeContext => ({ nodes: fleetNodes(fleet), selectedNodeDid, homeDid, fleet });

/* the nodes a scope names, in the snapshot's order */
export function nodesInScope(scope: Scope, ctx: ScopeContext): NodeLike[] {
  const { nodes } = ctx;
  if (scope.nodes === "all") return [...nodes];
  if (scope.nodes === "working" || scope.nodes === "selected") {
    /* with no node of its own the client works on whatever it selected */
    const w = scope.nodes === "working" ? workingNode(nodes, ctx.homeDid) : null;
    if (w) return [w];
    /* a fresh client has no nodes at all yet */
    const first = nodes[0];
    if (!first) return [];
    const did = ctx.selectedNodeDid ?? nodeDidOf(first) ?? null;
    return nodes.filter((n) => nodeDidOf(n) === did);
  }
  const chosen = new Set(scope.nodes);
  return nodes.filter((n) => chosen.has(nodeDidOf(n)));
}

/* A pick kept in local storage outlives its nodes: storage belongs to the
   app's origin, not to a home, so a reset home or a removed peer leaves
   DIDs no node has, which would narrow a list to nothing. Only DIDs of
   nodes this client has count; before any node arrives the pick stands. */
export function knownNodeIds(ids: readonly NodeDid[], ctx: ScopeContext): NodeDid[] {
  if (ctx.nodes.length === 0) return [...ids];
  const known = new Set(ctx.nodes.map(nodeDidOf));
  return ids.filter((id) => known.has(id));
}

const agentPasses = (scope: Scope, agentId: string | null | undefined) =>
  scope.agents.length === 0 || (agentId != null && scope.agents.includes(agentId));

/* every session the scope covers, across nodes, each still naming its node */
export function sessionsInScope(scope: Scope, ctx: ScopeContext): SessionSummary[] {
  return nodesInScope(scope, ctx).flatMap((n) =>
    (ctx.fleet.sessionsOf[nodeDidOf(n)] ?? []).filter((s) =>
      agentPasses(scope, s.behaviorId),
    ),
  );
}

/* open mailbox items the scope covers; the node is who filed them */
export function mailboxInScope(scope: Scope, ctx: ScopeContext): MailboxItemView[] {
  return nodesInScope(scope, ctx).flatMap((n) =>
    (ctx.fleet.mailboxOf[nodeDidOf(n)] ?? []).filter(
      (m) => m.status === "open" && agentPasses(scope, m.targetBehaviorId),
    ),
  );
}

/* the newest sessions the scope covers */
export function recentInScope(
  scope: Scope,
  ctx: ScopeContext,
  limit: number,
): SessionSummary[] {
  return sessionsInScope(scope, ctx)
    .sort((a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""))
    .slice(0, limit);
}
