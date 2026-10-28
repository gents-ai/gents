import type { MailboxItemView, SessionSummary } from "@source-inc/gents-desktop-client";
import type { FleetState, NodeView } from "../../hooks/fleetStore";
import { nodeDidOf, workingNode, type NodeDid, type NodeLike } from "./nodes";

export type NodeScope = "selected" | "working" | "all" | readonly NodeDid[];
export type Scope = { nodes: NodeScope; agents: readonly string[] };

export type ScopedView = "sessions" | "mailbox" | "recents";

export const defaultScope = (_view: ScopedView): Scope => ({
  nodes: "selected",
  agents: [],
});

export type ScopeContext = {
  nodes: readonly NodeLike[];
  selectedNodeDid: NodeDid | null;
  homeDid: string | null | undefined;
  fleet: Pick<FleetState, "sessionsOf" | "mailboxOf">;
};

export const fleetNodes = (fleet: FleetState): NodeView[] =>
  fleet.nodeKeys.flatMap((key) => fleet.nodes[key] ?? []);

export const scopeContextOf = (
  fleet: FleetState,
  selectedNodeDid: NodeDid | null,
  homeDid: string | null,
): ScopeContext => ({ nodes: fleetNodes(fleet), selectedNodeDid, homeDid, fleet });

export function nodesInScope(
  scope: Scope,
  ctx: Omit<ScopeContext, "fleet">,
): NodeLike[] {
  const { nodes } = ctx;
  if (scope.nodes === "all") return [...nodes];
  if (scope.nodes === "working" || scope.nodes === "selected") {
    const w = scope.nodes === "working" ? workingNode(nodes, ctx.homeDid) : null;
    if (w) return [w];
    const first = nodes[0];
    if (!first) return [];
    const did = ctx.selectedNodeDid ?? nodeDidOf(first) ?? null;
    return nodes.filter((n) => nodeDidOf(n) === did);
  }
  const chosen = new Set(scope.nodes);
  return nodes.filter((n) => chosen.has(nodeDidOf(n)));
}

export function knownNodeIds(
  ids: readonly NodeDid[],
  ctx: Pick<ScopeContext, "nodes">,
): NodeDid[] {
  if (ctx.nodes.length === 0) return [...ids];
  const known = new Set(ctx.nodes.map(nodeDidOf));
  return ids.filter((id) => known.has(id));
}

const agentPasses = (scope: Scope, agentId: string | null | undefined) =>
  scope.agents.length === 0 || (agentId != null && scope.agents.includes(agentId));

export function sessionsInScope(scope: Scope, ctx: ScopeContext): SessionSummary[] {
  return nodesInScope(scope, ctx).flatMap((n) =>
    (ctx.fleet.sessionsOf[nodeDidOf(n)] ?? []).filter((s) =>
      agentPasses(scope, s.agentId),
    ),
  );
}

export function mailboxInScope(scope: Scope, ctx: ScopeContext): MailboxItemView[] {
  return nodesInScope(scope, ctx).flatMap((n) =>
    (ctx.fleet.mailboxOf[nodeDidOf(n)] ?? []).filter(
      (m) => m.status === "open" && agentPasses(scope, m.targetAgentId),
    ),
  );
}

export function recentInScope(
  scope: Scope,
  ctx: ScopeContext,
  limit: number,
): SessionSummary[] {
  return sessionsInScope(scope, ctx)
    .sort((a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""))
    .slice(0, limit);
}
