import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";

import { listSubagentTree } from "../../lib/desktop-api";
import type {
  SubagentEdgeView,
  SubagentNodeView,
  SubagentTreeView,
} from "../../lib/types";

export type SubagentLineageViewProps = {
  rootRequestId: string | null;
  agentDid: string | null;
};

type RequestNode = {
  kind: "req";
  id: string;
  node: SubagentNodeView;
  children: ToolNode[];
};

type ToolNode = {
  kind: "tool";
  id: string;
  edge: SubagentEdgeView;
  child: RequestNode | null;
};

type AnyNode = RequestNode | ToolNode;

type Selected =
  | { kind: "req"; node: SubagentNodeView }
  | { kind: "tool"; edge: SubagentEdgeView }
  | null;

const TERMINAL_STATES = new Set([
  "completed",
  "failed",
  "cancelled",
  "interrupted",
  "superseded",
  "dead",
]);

function lifecycleIsLive(state?: string | null) {
  if (!state) return true;
  return !TERMINAL_STATES.has(state.toLowerCase());
}

function nodeId(node: AnyNode): string {
  return `${node.kind}:${node.id}`;
}

function shortenDid(did?: string | null): string {
  if (!did) return "—";
  if (did.length <= 24) return did;
  return `${did.slice(0, 20)}…${did.slice(-4)}`;
}

function buildTree(view: SubagentTreeView): {
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
    const children = (edgesByParent.get(requestId) ?? []).map<ToolNode>((edge) => ({
      kind: "tool",
      id: edge.parentToolCallId ?? `${edge.parentRequestId}->${edge.childRequestId}`,
      edge,
      child: buildRequest(edge.childRequestId),
    }));
    return { kind: "req", id: requestId, node: view, children };
  }

  const root = buildRequest(view.rootRequestId);
  return { root, deployments: [...deployments].sort() };
}

function depthOfRequest(node: SubagentNodeView, fallback: number): number {
  return node.subagentDepth ?? fallback;
}

function subtreeHasSurvivor(
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

type FilterState = {
  depth: "all" | 0 | 1 | 2 | 3;
  deployments: Set<string>;
  liveOnly: boolean;
};

function nodePasses(node: RequestNode, depth: number, state: FilterState): boolean {
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

function flattenTreeOrder(root: RequestNode | null, expanded: Set<string>): AnyNode[] {
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

export function SubagentLineageView({
  rootRequestId,
  agentDid,
}: SubagentLineageViewProps) {
  const [tree, setTree] = useState<SubagentTreeView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [depthFilter, setDepthFilter] = useState<FilterState["depth"]>("all");
  const [liveOnly, setLiveOnly] = useState(false);
  const [deploymentFilter, setDeploymentFilter] = useState<Set<string>>(new Set());

  const treeContainerRef = useRef<HTMLUListElement | null>(null);

  // Fetch when root or agent changes.
  useEffect(() => {
    let cancelled = false;
    if (!rootRequestId) {
      setTree(null);
      setError(null);
      setLoading(false);
      return;
    }
    setLoading(true);
    setError(null);
    listSubagentTree({ rootRequestId, agentDid: agentDid ?? null })
      .then((value) => {
        if (cancelled) return;
        setTree(value);
        // Expand everything by default so the panel shows useful data on load.
        const next = new Set<string>();
        for (const node of value.nodes) next.add(`req:${node.requestId}`);
        for (const edge of value.edges) {
          next.add(
            `tool:${edge.parentToolCallId ?? `${edge.parentRequestId}->${edge.childRequestId}`}`,
          );
        }
        setExpanded(next);
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        setTree(null);
        setError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [rootRequestId, agentDid]);

  const { rootBuilt, deployments } = useMemo(() => {
    if (!tree) return { rootBuilt: null, deployments: [] };
    const { root, deployments } = buildTree(tree);
    return { rootBuilt: root, deployments };
  }, [tree]);

  // Prune the deployment filter set when scenario changes underneath us so
  // stale ids don't silently hide everything.
  useEffect(() => {
    setDeploymentFilter((current) => {
      const available = new Set(deployments);
      const next = new Set<string>();
      for (const value of current) if (available.has(value)) next.add(value);
      return next.size === current.size ? current : next;
    });
  }, [deployments]);

  const filterState: FilterState = useMemo(
    () => ({
      depth: depthFilter,
      deployments: deploymentFilter,
      liveOnly,
    }),
    [depthFilter, deploymentFilter, liveOnly],
  );

  const visibleOrder = useMemo(
    () => flattenTreeOrder(rootBuilt, expanded),
    [rootBuilt, expanded],
  );

  const toggleExpanded = useCallback((id: string) => {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const focusNode = useCallback((id: string) => {
    const container = treeContainerRef.current;
    if (!container) return;
    const target = container.querySelector<HTMLDivElement>(
      `[data-node-id="${cssEscape(id)}"]`,
    );
    if (target) target.focus({ preventScroll: false });
  }, []);

  const selectNode = useCallback(
    (node: AnyNode, focus: boolean) => {
      const id = nodeId(node);
      setSelectedId(id);
      if (focus) focusNode(id);
    },
    [focusNode],
  );

  const move = useCallback(
    (delta: number) => {
      if (visibleOrder.length === 0) return;
      const ids = visibleOrder.map(nodeId);
      let idx = selectedId ? ids.indexOf(selectedId) : -1;
      idx = idx < 0 ? 0 : Math.max(0, Math.min(ids.length - 1, idx + delta));
      const target = visibleOrder[idx];
      if (target) selectNode(target, true);
    },
    [visibleOrder, selectedId, selectNode],
  );

  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLUListElement>) => {
      switch (event.key) {
        case "ArrowDown":
          event.preventDefault();
          move(1);
          break;
        case "ArrowUp":
          event.preventDefault();
          move(-1);
          break;
        case "ArrowRight": {
          event.preventDefault();
          if (!selectedId) return;
          if (!expanded.has(selectedId)) toggleExpanded(selectedId);
          else move(1);
          break;
        }
        case "ArrowLeft": {
          event.preventDefault();
          if (!selectedId) return;
          if (expanded.has(selectedId)) toggleExpanded(selectedId);
          break;
        }
        case "Home":
          event.preventDefault();
          if (visibleOrder[0]) selectNode(visibleOrder[0], true);
          break;
        case "End":
          event.preventDefault();
          {
            const last = visibleOrder[visibleOrder.length - 1];
            if (last) selectNode(last, true);
          }
          break;
        case "Enter":
        case " ":
          event.preventDefault();
          if (selectedId) toggleExpanded(selectedId);
          break;
        default:
          break;
      }
    },
    [move, expanded, selectedId, toggleExpanded, selectNode, visibleOrder],
  );

  const selected: Selected = useMemo(() => {
    if (!selectedId || !tree) return null;
    const [kind, id] = splitNodeId(selectedId);
    if (kind === "req") {
      const node = tree.nodes.find((n) => n.requestId === id);
      return node ? { kind: "req", node } : null;
    }
    const edge = tree.edges.find(
      (e) => (e.parentToolCallId ?? `${e.parentRequestId}->${e.childRequestId}`) === id,
    );
    return edge ? { kind: "tool", edge } : null;
  }, [selectedId, tree]);

  return (
    <div className="subagent-lineage" aria-label="Subagent lineage">
      <header className="subagent-lineage-header">
        <h2>Lineage</h2>
        <div className="subagent-lineage-meta">
          {tree
            ? `root ${tree.rootRequestId} · ${tree.nodes.length} req · ${tree.edges.length} bridge${tree.truncated ? " · truncated" : ""}`
            : rootRequestId
              ? loading
                ? "loading…"
                : "—"
              : "no active turn"}
        </div>
      </header>

      <div
        className="subagent-lineage-filters"
        role="toolbar"
        aria-label="Lineage filters"
      >
        <span className="subagent-lineage-filter-label">Depth</span>
        {(["all", 0, 1, 2, 3] as const).map((value) => (
          <button
            key={String(value)}
            type="button"
            className="subagent-lineage-chip"
            aria-pressed={depthFilter === value}
            onClick={() => setDepthFilter(value)}
          >
            {value === "all" ? "All" : value === 0 ? "0" : `≤${value}`}
          </button>
        ))}
        {deployments.length > 0 ? (
          <>
            <span className="subagent-lineage-filter-label">Deployment</span>
            {deployments.map((did) => {
              const active = deploymentFilter.has(did);
              return (
                <button
                  key={did}
                  type="button"
                  className="subagent-lineage-chip"
                  aria-pressed={active}
                  title={did}
                  onClick={() =>
                    setDeploymentFilter((current) => {
                      const next = new Set(current);
                      if (next.has(did)) next.delete(did);
                      else next.add(did);
                      return next;
                    })
                  }
                >
                  {shortenDid(did)}
                </button>
              );
            })}
          </>
        ) : null}
        <label className="subagent-lineage-live-toggle">
          <input
            type="checkbox"
            checked={liveOnly}
            onChange={(event) => setLiveOnly(event.target.checked)}
          />
          Live only
        </label>
      </div>

      <div className="subagent-lineage-body">
        <section className="subagent-lineage-tree-pane" aria-label="Lineage tree">
          {loading ? (
            <p className="subagent-lineage-empty">Loading lineage…</p>
          ) : error ? (
            <p className="subagent-lineage-empty subagent-lineage-error" role="alert">
              {error}
            </p>
          ) : !rootRequestId ? (
            <p className="subagent-lineage-empty">
              Open a session with a recent turn to see its subagent lineage.
            </p>
          ) : !rootBuilt ? (
            <p className="subagent-lineage-empty">No active subagent dispatches.</p>
          ) : (
            <ul
              role="tree"
              aria-label="Subagent lineage"
              tabIndex={0}
              className="subagent-lineage-tree"
              ref={treeContainerRef}
              onKeyDown={handleKeyDown}
            >
              <SubagentTreeRow
                node={rootBuilt}
                depth={0}
                expanded={expanded}
                selectedId={selectedId}
                filter={filterState}
                onToggle={toggleExpanded}
                onSelect={selectNode}
              />
            </ul>
          )}
        </section>

        <aside className="subagent-lineage-detail" aria-label="Selected node detail">
          <SubagentDetailPanel selected={selected} />
        </aside>
      </div>
    </div>
  );
}

type SubagentTreeRowProps = {
  node: AnyNode;
  depth: number;
  expanded: Set<string>;
  selectedId: string | null;
  filter: FilterState;
  onToggle: (id: string) => void;
  onSelect: (node: AnyNode, focus: boolean) => void;
};

function SubagentTreeRow({
  node,
  depth,
  expanded,
  selectedId,
  filter,
  onToggle,
  onSelect,
}: SubagentTreeRowProps) {
  if (node.kind === "req") {
    if (!subtreeHasSurvivor(node, depth, filter)) return null;
  }
  const id = nodeId(node);
  const isExpanded = expanded.has(id);
  const isSelected = selectedId === id;
  const hasChildren =
    node.kind === "req" ? node.children.length > 0 : Boolean(node.child);
  const childDepth = node.kind === "req" ? depth + 1 : depth;

  return (
    <li
      role="treeitem"
      aria-expanded={hasChildren ? isExpanded : undefined}
      aria-selected={isSelected || undefined}
      className="subagent-lineage-tree-item"
    >
      <div
        className={"subagent-lineage-row" + (isSelected ? " is-selected" : "")}
        data-node-id={id}
        tabIndex={isSelected ? 0 : -1}
        onClick={() => onSelect(node, false)}
        onFocus={() => onSelect(node, false)}
      >
        {hasChildren ? (
          <button
            type="button"
            className="subagent-lineage-twisty"
            aria-expanded={isExpanded}
            aria-label={isExpanded ? "Collapse" : "Expand"}
            onClick={(event) => {
              event.stopPropagation();
              onToggle(id);
            }}
          >
            <svg
              viewBox="0 0 8 8"
              width="8"
              height="8"
              aria-hidden="true"
              style={{
                transform: isExpanded ? "rotate(0deg)" : "rotate(-90deg)",
                transition: "transform 120ms ease",
              }}
            >
              <path
                d="M0.5 2 L4 6 L7.5 2"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.4"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
          </button>
        ) : (
          <span className="subagent-lineage-twisty subagent-lineage-twisty-placeholder" />
        )}
        <span className="subagent-lineage-label">
          <span
            className={
              "subagent-lineage-kind " +
              (node.kind === "req"
                ? "subagent-lineage-kind-req"
                : "subagent-lineage-kind-tool")
            }
          >
            {node.kind === "req" ? "req" : "tool"}
          </span>
          <span className="subagent-lineage-id">
            {node.kind === "req"
              ? node.node.requestId
              : (node.edge.parentToolCallId ?? "—")}
          </span>
          <span className="subagent-lineage-secondary">{nodeSecondary(node)}</span>
        </span>
        {nodeBadge(node)}
      </div>
      {hasChildren && isExpanded ? (
        <ul role="group" className="subagent-lineage-children">
          {node.kind === "req" ? (
            node.children.map((child) => (
              <SubagentTreeRow
                key={nodeId(child)}
                node={child}
                depth={childDepth}
                expanded={expanded}
                selectedId={selectedId}
                filter={filter}
                onToggle={onToggle}
                onSelect={onSelect}
              />
            ))
          ) : node.child ? (
            <SubagentTreeRow
              key={nodeId(node.child)}
              node={node.child}
              depth={childDepth}
              expanded={expanded}
              selectedId={selectedId}
              filter={filter}
              onToggle={onToggle}
              onSelect={onSelect}
            />
          ) : null}
        </ul>
      ) : null}
    </li>
  );
}

function nodeSecondary(node: AnyNode): string {
  if (node.kind === "req") {
    const bits = [
      node.node.behaviorId,
      node.node.agentDid ? shortenDid(node.node.agentDid) : null,
    ].filter(Boolean);
    return bits.join("  ·  ");
  }
  const bits = [
    node.edge.toolName ?? "spawn_subagent",
    [node.edge.awaitMode, node.edge.cancelPolicy].filter(Boolean).join("/") || null,
  ].filter(Boolean);
  return bits.join("  ·  ");
}

function nodeBadge(node: AnyNode) {
  const value =
    node.kind === "req"
      ? (node.node.lifecycleState ?? node.node.status ?? null)
      : (node.edge.lifecycleState ?? null);
  if (!value) return null;
  const safe = value.toLowerCase();
  return (
    <span
      className={`subagent-lineage-state subagent-lineage-state-${safe}`}
      aria-label={`state ${value}`}
    >
      {value}
    </span>
  );
}

function SubagentDetailPanel({ selected }: { selected: Selected }) {
  if (!selected) {
    return (
      <p className="subagent-lineage-empty">
        Select a node in the lineage tree to see request or bridge metadata.
      </p>
    );
  }
  if (selected.kind === "req") {
    const node = selected.node;
    return (
      <dl className="subagent-lineage-detail-grid">
        <DetailRow label="request id" value={node.requestId} />
        <DetailRow label="session" value={node.sessionId} />
        <DetailRow label="deployment" value={node.agentDid} mono />
        <DetailRow label="behavior" value={node.behaviorId} />
        <DetailRow label="lifecycle" value={node.lifecycleState} badge />
        <DetailRow label="status" value={node.status} />
        <DetailRow
          label="depth"
          value={node.subagentDepth != null ? String(node.subagentDepth) : null}
        />
        <DetailRow label="parent req" value={node.causedByParentRequestId} />
        <DetailRow label="parent tool" value={node.causedByParentToolCallId} />
      </dl>
    );
  }
  const edge = selected.edge;
  return (
    <dl className="subagent-lineage-detail-grid">
      <DetailRow label="tool call id" value={edge.parentToolCallId} />
      <DetailRow label="parent req" value={edge.parentRequestId} />
      <DetailRow label="child req" value={edge.childRequestId} />
      <DetailRow label="tool" value={edge.toolName} />
      <DetailRow label="await mode" value={edge.awaitMode} />
      <DetailRow label="cancel policy" value={edge.cancelPolicy} />
      <DetailRow label="lifecycle" value={edge.lifecycleState} badge />
    </dl>
  );
}

function DetailRow({
  label,
  value,
  badge,
  mono,
}: {
  label: string;
  value: string | null | undefined;
  badge?: boolean;
  mono?: boolean;
}) {
  const hasValue = Boolean(value && value.length > 0);
  return (
    <>
      <dt>{label}</dt>
      <dd className={hasValue ? (mono ? "is-mono" : "") : "is-muted"}>
        {hasValue ? (
          badge ? (
            <span
              className={`subagent-lineage-state subagent-lineage-state-${(value ?? "").toLowerCase()}`}
            >
              {value}
            </span>
          ) : (
            value
          )
        ) : (
          "—"
        )}
      </dd>
    </>
  );
}

function cssEscape(value: string): string {
  if (typeof window !== "undefined" && window.CSS && CSS.escape) {
    return CSS.escape(value);
  }
  return value.replace(/([^\w-])/g, "\\$1");
}

function splitNodeId(id: string): ["req" | "tool", string] {
  const idx = id.indexOf(":");
  if (idx < 0) return ["req", id];
  const kind = id.slice(0, idx);
  const rest = id.slice(idx + 1);
  return [kind === "tool" ? "tool" : "req", rest];
}
