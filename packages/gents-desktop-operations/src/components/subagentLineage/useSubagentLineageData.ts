import { useCallback, useEffect, useMemo, useState } from "react";

import type {
  DesktopApiAdapter,
  SubagentTreeView,
} from "@source-inc/gents-desktop-client";
import type { FilterState } from "./lineageModel.js";
import { buildTree } from "./lineageModel.js";

export function useSubagentLineageData({
  agentDid,
  api,
  rootRequestId,
}: {
  agentDid: string | null;
  api: DesktopApiAdapter;
  rootRequestId: string | null;
}) {
  const [tree, setTree] = useState<SubagentTreeView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [depthFilter, setDepthFilter] = useState<FilterState["depth"]>("all");
  const [liveOnly, setLiveOnly] = useState(false);
  const [deploymentFilter, setDeploymentFilter] = useState<Set<string>>(
    new Set(),
  );

  // The lineage observes a live turn, so keep polling while mounted. Background
  // refreshes preserve operator choices and only expand genuinely new nodes.
  useEffect(() => {
    let cancelled = false;
    if (!rootRequestId) {
      setTree(null);
      setError(null);
      setLoading(false);
      return;
    }

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
        setError(null);
        const keys = treeKeys(value);
        setTree(value);
        if (seenKeys === null) {
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
      } catch (caught) {
        if (cancelled || background) return;
        setTree(null);
        setError(caught instanceof Error ? caught.message : String(caught));
      } finally {
        if (!cancelled && !background) setLoading(false);
      }
    };

    void fetchTree(false);
    const handle = window.setInterval(() => void fetchTree(true), 5_000);
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

  // A changing scenario must not retain ids that silently hide every node.
  useEffect(() => {
    setDeploymentFilter((current) => {
      const available = new Set(deployments);
      const next = new Set<string>();
      for (const value of current) {
        if (available.has(value)) next.add(value);
      }
      return next.size === current.size ? current : next;
    });
  }, [deployments]);

  const filterState = useMemo<FilterState>(
    () => ({
      depth: depthFilter,
      deployments: deploymentFilter,
      liveOnly,
    }),
    [depthFilter, deploymentFilter, liveOnly],
  );

  const toggleExpanded = useCallback((id: string) => {
    setExpanded((current) => {
      const next = new Set(current);
      next.has(id) ? next.delete(id) : next.add(id);
      return next;
    });
  }, []);

  const toggleDeployment = useCallback((did: string) => {
    setDeploymentFilter((current) => {
      const next = new Set(current);
      next.has(did) ? next.delete(did) : next.add(did);
      return next;
    });
  }, []);

  return {
    deploymentFilter,
    deployments,
    depthFilter,
    error,
    expanded,
    filterState,
    liveOnly,
    loading,
    rootBuilt,
    tree,
    setDepthFilter,
    setLiveOnly,
    toggleDeployment,
    toggleExpanded,
  };
}

function treeKeys(tree: SubagentTreeView) {
  const keys = new Set<string>();
  for (const node of tree.nodes) keys.add(`req:${node.requestId}`);
  for (const edge of tree.edges) {
    keys.add(
      `tool:${edge.parentToolCallId ?? `${edge.parentRequestId}->${edge.childRequestId}`}`,
    );
  }
  return keys;
}
