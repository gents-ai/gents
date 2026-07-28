import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { useOperationsApi } from "../../apiContext.js";
import { SubagentLineageBody } from "./SubagentLineageBody.js";
import { SubagentLineageFilters } from "./SubagentLineageFilters.js";
import { useSubagentLineageData } from "./useSubagentLineageData.js";
import { useSubagentLineageNavigation } from "./useSubagentLineageNavigation.js";

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
  const data = useSubagentLineageData({ agentDid, api, rootRequestId });
  const navigation = useSubagentLineageNavigation({
    expanded: data.expanded,
    root: data.rootBuilt,
    tree: data.tree,
    onToggleExpanded: data.toggleExpanded,
  });

  return (
    <div className="subagent-lineage" aria-label="Subagent lineage">
      <header className="subagent-lineage-header">
        <h2>Lineage</h2>
        <div className="subagent-lineage-meta">
          {data.tree
            ? `root ${data.tree.rootRequestId} · ${data.tree.nodes.length} req · ${data.tree.edges.length} bridge${data.tree.truncated ? " · truncated" : ""}`
            : rootRequestId
              ? data.loading
                ? "loading…"
                : "—"
              : "no active turn"}
        </div>
      </header>
      {data.tree?.partialErrors?.length ? (
        <p
          className="subagent-lineage-partial"
          data-testid="lineage-partial-errors"
          role="alert"
        >
          Some deployments could not be queried — branches may be missing:{" "}
          {data.tree.partialErrors.join("; ")}
        </p>
      ) : null}
      <SubagentLineageFilters
        deploymentFilter={data.deploymentFilter}
        deployments={data.deployments}
        depthFilter={data.depthFilter}
        liveOnly={data.liveOnly}
        onDepthFilterChange={data.setDepthFilter}
        onDeploymentToggle={data.toggleDeployment}
        onLiveOnlyChange={data.setLiveOnly}
      />
      <SubagentLineageBody
        error={data.error}
        expanded={data.expanded}
        filter={data.filterState}
        loading={data.loading}
        root={data.rootBuilt}
        rootRequestId={rootRequestId}
        selected={navigation.selected}
        selectedId={navigation.selectedId}
        treeRef={navigation.treeContainerRef}
        onKeyDown={navigation.handleKeyDown}
        onSelect={navigation.selectNode}
        onToggle={data.toggleExpanded}
      />
    </div>
  );
}
