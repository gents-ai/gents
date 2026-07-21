import { shortId } from "../../lib/shortId";
import type { AnyNode, FilterState } from "./lineageModel";
import { nodeId, shortenDid, subtreeHasSurvivor } from "./lineageModel";

export type SubagentTreeRowProps = {
  node: AnyNode;
  depth: number;
  expanded: Set<string>;
  selectedId: string | null;
  filter: FilterState;
  onToggle: (id: string) => void;
  onSelect: (node: AnyNode, focus: boolean) => void;
};

export function SubagentTreeRow({
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
          <span
            className="subagent-lineage-id"
            title={
              node.kind === "req"
                ? node.node.requestId
                : (node.edge.parentToolCallId ?? undefined)
            }
          >
            {node.kind === "req"
              ? shortId(node.node.requestId)
              : node.edge.parentToolCallId
                ? shortId(node.edge.parentToolCallId)
                : "—"}
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
