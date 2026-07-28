import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";

import type {
  DesktopApiAdapter,
  SubagentTreeView,
} from "@source-inc/gents-desktop-client";
import { useOperationsApi } from "../../apiContext.js";
import { SubagentDetailPanel } from "./SubagentDetailPanel.js";
import { SubagentTreeRow } from "./SubagentLineageTree.js";
import type { AnyNode, FilterState, Selected } from "./lineageModel.js";
import {
  buildTree,
  flattenTreeOrder,
  nodeId,
  shortenDid,
  splitNodeId,
} from "./lineageModel.js";

export type SubagentLineageViewProps = {
  rootRequestId: string | null;
  agentDid: string | null;
  api?: DesktopApiAdapter;
};

export function SubagentLineageView({
  rootRequestId,
  agentDid,
  api: explicitApi,
}: SubagentLineageViewProps) {
  const api = useOperationsApi(explicitApi);
  const [tree, setTree] = useState<SubagentTreeView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [depthFilter, setDepthFilter] = useState<FilterState["depth"]>("all");
  const [liveOnly, setLiveOnly] = useState(false);
  const [deploymentFilter, setDeploymentFilter] = useState<Set<string>>(
    new Set(),
  );

  const treeContainerRef = useRef<HTMLUListElement | null>(null);

  // Fetch when root or agent changes, then keep polling while mounted —
  // the lineage exists to observe a LIVE turn, so a one-shot snapshot
  // going stale defeats it. Background refreshes preserve the operator's
  // expand/collapse choices and only auto-expand genuinely new nodes.
  useEffect(() => {
    let cancelled = false;
    if (!rootRequestId) {
      setTree(null);
      setError(null);
      setLoading(false);
      return;
    }

    const treeKeys = (value: SubagentTreeView) => {
      const keys = new Set<string>();
      for (const node of value.nodes) keys.add(`req:${node.requestId}`);
      for (const edge of value.edges) {
        keys.add(
          `tool:${edge.parentToolCallId ?? `${edge.parentRequestId}->${edge.childRequestId}`}`,
        );
      }
      return keys;
    };
    let seenKeys: Set<string> | null = null;

    const fetchTree = async (background: boolean) => {
      if (!background) {
        setLoading(true);
        setError(null);
      }
      try {
        const value = await api.listSubagentTree({
          rootRequestId,
          agentDid: agentDid ?? null,
          includeTerminal: true,
        });
        if (cancelled) return;
        // A successful background retry must recover the visible panel after
        // an initial transient failure; otherwise the stale error branch keeps
        // winning even though a fresh tree has arrived.
        setError(null);
        const keys = treeKeys(value);
        setTree(value);
        if (seenKeys === null) {
          // Expand everything by default so the panel shows useful data.
          setExpanded(keys);
        } else {
          const previous = seenKeys;
          setExpanded((current) => {
            const next = new Set(current);
            for (const key of keys) {
              if (!previous.has(key)) next.add(key);
            }
            return next;
          });
        }
        seenKeys = keys;
      } catch (error: unknown) {
        if (cancelled || background) return;
        setTree(null);
        setError(error instanceof Error ? error.message : String(error));
      } finally {
        if (!cancelled && !background) setLoading(false);
      }
    };

    void fetchTree(false);
    const handle = window.setInterval(() => {
      void fetchTree(true);
    }, 5_000);
    return () => {
      cancelled = true;
      window.clearInterval(handle);
    };
  }, [agentDid, api, rootRequestId]);

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
      (e) =>
        (e.parentToolCallId ?? `${e.parentRequestId}->${e.childRequestId}`) ===
        id,
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

      {tree?.partialErrors?.length ? (
        <p
          className="subagent-lineage-partial"
          data-testid="lineage-partial-errors"
          role="alert"
        >
          Some deployments could not be queried — branches may be missing:{" "}
          {tree.partialErrors.join("; ")}
        </p>
      ) : null}
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
        <section
          className="subagent-lineage-tree-pane"
          aria-label="Lineage tree"
        >
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
          ) : !rootBuilt ? (
            <p className="subagent-lineage-empty">
              No active subagent dispatches.
            </p>
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

        <aside
          className="subagent-lineage-detail"
          aria-label="Selected node detail"
        >
          <SubagentDetailPanel selected={selected} />
        </aside>
      </div>
    </div>
  );
}

function cssEscape(value: string): string {
  if (typeof window !== "undefined" && window.CSS && CSS.escape) {
    return CSS.escape(value);
  }
  return value.replace(/([^\w-])/g, "\\$1");
}
