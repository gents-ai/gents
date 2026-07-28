import type {
  SubagentEdgeView,
  SubagentNodeView,
  SubagentTreeView,
} from "@source-inc/gents-desktop-client";

export type RequestNode = {
  kind: "req";
  id: string;
  node: SubagentNodeView;
  children: ToolNode[];
};

export type ToolNode = {
  kind: "tool";
  id: string;
  edge: SubagentEdgeView;
  child: RequestNode | null;
};

export type AnyNode = RequestNode | ToolNode;

export type Selected =
  | { kind: "req"; node: SubagentNodeView }
  | { kind: "tool"; edge: SubagentEdgeView }
  | null;

export type FilterState = {
  depth: "all" | 0 | 1 | 2 | 3;
  deployments: Set<string>;
  liveOnly: boolean;
};

const TERMINAL_STATES = new Set([
  "completed",
  "failed",
  "cancelled",
  "interrupted",
  "superseded",
  "dead",
]);

export function nodeId(node: AnyNode): string {
  return `${node.kind}:${node.id}`;
}

export function shortenDid(did?: string | null): string {
  if (!did) return "—";
  if (did.length <= 24) return did;
  return `${did.slice(0, 20)}…${did.slice(-4)}`;
}

export function buildTree(view: SubagentTreeView): {
  root: RequestNode | null;
  deployments: string[];
} {
  const nodeMap = new Map<string, SubagentNodeView>();
  for (const node of view.nodes) {
    nodeMap.set(node.requestId, node);
  }
  const edgesByParent = new Map<string, SubagentEdgeView[]>();
  for (const edge of view.edges) {
    const list = edgesByParent.get(edge.parentRequestId) ?? [];
    list.push(edge);
    edgesByParent.set(edge.parentRequestId, list);
  }
  const visited = new Set<string>();
  const deployments = new Set<string>();

  function buildRequest(requestId: string): RequestNode | null {
    if (visited.has(requestId)) return null;
    visited.add(requestId);
    const view = nodeMap.get(requestId);
    if (!view) return null;
    if (view.agentDid) deployments.add(view.agentDid);
    const children = (edgesByParent.get(requestId) ?? []).map<ToolNode>(
      (edge) => ({
        kind: "tool",
        id:
          edge.parentToolCallId ??
          `${edge.parentRequestId}->${edge.childRequestId}`,
        edge,
        child: buildRequest(edge.childRequestId),
      }),
    );
    return { kind: "req", id: requestId, node: view, children };
  }

  const root = buildRequest(view.rootRequestId);
  return { root, deployments: [...deployments].sort() };
}

export function subtreeHasSurvivor(
  node: RequestNode,
  depth: number,
  state: FilterState,
): boolean {
  const passes = nodePasses(node, depth, state);
  if (
    passes &&
    (state.deployments.size === 0 ||
      (node.node.agentDid && state.deployments.has(node.node.agentDid)))
  ) {
    return true;
  }
  for (const tool of node.children) {
    if (tool.child && subtreeHasSurvivor(tool.child, depth + 1, state)) {
      return true;
    }
  }
  return passes;
}

export function flattenTreeOrder(
  root: RequestNode | null,
  expanded: Set<string>,
): AnyNode[] {
  const out: AnyNode[] = [];
  function walk(node: AnyNode) {
    out.push(node);
    if (!expanded.has(nodeId(node))) return;
    if (node.kind === "req") {
      for (const child of node.children) walk(child);
    } else if (node.child) {
      walk(node.child);
    }
  }
  if (root) walk(root);
  return out;
}

export function splitNodeId(id: string): ["req" | "tool", string] {
  const idx = id.indexOf(":");
  if (idx < 0) return ["req", id];
  const kind = id.slice(0, idx);
  const rest = id.slice(idx + 1);
  return [kind === "tool" ? "tool" : "req", rest];
}

function lifecycleIsLive(state?: string | null) {
  if (!state) return true;
  return !TERMINAL_STATES.has(state.toLowerCase());
}

function depthOfRequest(node: SubagentNodeView, fallback: number): number {
  return node.subagentDepth ?? fallback;
}

function nodePasses(
  node: RequestNode,
  depth: number,
  state: FilterState,
): boolean {
  if (state.depth !== "all") {
    const max = state.depth;
    if (depthOfRequest(node.node, depth) > max) return false;
  }
  if (state.liveOnly && !lifecycleIsLive(node.node.lifecycleState)) {
    // Only keep if any descendant survives; that check happens at render time.
    return false;
  }
  if (state.deployments.size > 0) {
    if (!node.node.agentDid || !state.deployments.has(node.node.agentDid)) {
      return false;
    }
  }
  return true;
}
