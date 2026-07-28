import type { KeyboardEvent, RefObject } from "react";

import { SubagentDetailPanel } from "./SubagentDetailPanel.js";
import { SubagentTreeRow } from "./SubagentLineageTree.js";
import type {
  AnyNode,
  FilterState,
  RequestNode,
  Selected,
} from "./lineageModel.js";

export function SubagentLineageBody({
  error,
  expanded,
  filter,
  loading,
  root,
  rootRequestId,
  selected,
  selectedId,
  treeRef,
  onKeyDown,
  onSelect,
  onToggle,
}: {
  error: string | null;
  expanded: Set<string>;
  filter: FilterState;
  loading: boolean;
  root: RequestNode | null;
  rootRequestId: string | null;
  selected: Selected;
  selectedId: string | null;
  treeRef: RefObject<HTMLUListElement | null>;
  onKeyDown: (event: KeyboardEvent<HTMLUListElement>) => void;
  onSelect: (node: AnyNode, focus: boolean) => void;
  onToggle: (id: string) => void;
}) {
  return (
    <div className="subagent-lineage-body">
      <section className="subagent-lineage-tree-pane" aria-label="Lineage tree">
        {loading ? (
          <p className="subagent-lineage-empty">Loading lineage…</p>
        ) : error ? (
          <p
            className="subagent-lineage-empty subagent-lineage-error"
            role="alert"
          >
            {error}
          </p>
        ) : !rootRequestId ? (
          <p className="subagent-lineage-empty">
            Open a session with a recent turn to see its subagent lineage.
          </p>
        ) : !root ? (
          <p className="subagent-lineage-empty">
            No active subagent dispatches.
          </p>
        ) : (
          <ul
            role="tree"
            aria-label="Subagent lineage"
            tabIndex={0}
            className="subagent-lineage-tree"
            ref={treeRef}
            onKeyDown={onKeyDown}
          >
            <SubagentTreeRow
              node={root}
              depth={0}
              expanded={expanded}
              selectedId={selectedId}
              filter={filter}
              onToggle={onToggle}
              onSelect={onSelect}
            />
          </ul>
        )}
      </section>
      <aside
        className="subagent-lineage-detail"
        aria-label="Selected node detail"
      >
        <SubagentDetailPanel selected={selected} />
      </aside>
    </div>
  );
}
