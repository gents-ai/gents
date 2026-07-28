import {
  useCallback,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";

import type { SubagentTreeView } from "@source-inc/gents-desktop-client";
import type { AnyNode, RequestNode, Selected } from "./lineageModel.js";
import { flattenTreeOrder, nodeId, splitNodeId } from "./lineageModel.js";

export function useSubagentLineageNavigation({
  expanded,
  root,
  tree,
  onToggleExpanded,
}: {
  expanded: Set<string>;
  root: RequestNode | null;
  tree: SubagentTreeView | null;
  onToggleExpanded: (id: string) => void;
}) {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const treeContainerRef = useRef<HTMLUListElement | null>(null);
  const visibleOrder = useMemo(
    () => flattenTreeOrder(root, expanded),
    [expanded, root],
  );

  const focusNode = useCallback((id: string) => {
    const target = treeContainerRef.current?.querySelector<HTMLDivElement>(
      `[data-node-id="${cssEscape(id)}"]`,
    );
    target?.focus({ preventScroll: false });
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
      let index = selectedId ? ids.indexOf(selectedId) : -1;
      index =
        index < 0 ? 0 : Math.max(0, Math.min(ids.length - 1, index + delta));
      const target = visibleOrder[index];
      if (target) selectNode(target, true);
    },
    [selectedId, selectNode, visibleOrder],
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
        case "ArrowRight":
          event.preventDefault();
          if (!selectedId) return;
          if (!expanded.has(selectedId)) onToggleExpanded(selectedId);
          else move(1);
          break;
        case "ArrowLeft":
          event.preventDefault();
          if (selectedId && expanded.has(selectedId)) {
            onToggleExpanded(selectedId);
          }
          break;
        case "Home":
          event.preventDefault();
          if (visibleOrder[0]) selectNode(visibleOrder[0], true);
          break;
        case "End": {
          event.preventDefault();
          const last = visibleOrder[visibleOrder.length - 1];
          if (last) selectNode(last, true);
          break;
        }
        case "Enter":
        case " ":
          event.preventDefault();
          if (selectedId) onToggleExpanded(selectedId);
          break;
        default:
          break;
      }
    },
    [expanded, move, onToggleExpanded, selectedId, selectNode, visibleOrder],
  );

  const selected: Selected = useMemo(() => {
    if (!selectedId || !tree) return null;
    const [kind, id] = splitNodeId(selectedId);
    if (kind === "req") {
      const node = tree.nodes.find((entry) => entry.requestId === id);
      return node ? { kind: "req", node } : null;
    }
    const edge = tree.edges.find(
      (entry) =>
        (entry.parentToolCallId ??
          `${entry.parentRequestId}->${entry.childRequestId}`) === id,
    );
    return edge ? { kind: "tool", edge } : null;
  }, [selectedId, tree]);

  return {
    handleKeyDown,
    selected,
    selectedId,
    selectNode,
    treeContainerRef,
  };
}

function cssEscape(value: string): string {
  if (typeof window !== "undefined" && window.CSS && CSS.escape) {
    return CSS.escape(value);
  }
  return value.replace(/([^\w-])/g, "\\$1");
}
