import type { FilterState } from "./lineageModel.js";
import { shortenDid } from "./lineageModel.js";

export function SubagentLineageFilters({
  deploymentFilter,
  deployments,
  depthFilter,
  liveOnly,
  onDepthFilterChange,
  onDeploymentToggle,
  onLiveOnlyChange,
}: {
  deploymentFilter: Set<string>;
  deployments: string[];
  depthFilter: FilterState["depth"];
  liveOnly: boolean;
  onDepthFilterChange: (value: FilterState["depth"]) => void;
  onDeploymentToggle: (did: string) => void;
  onLiveOnlyChange: (value: boolean) => void;
}) {
  return (
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
          onClick={() => onDepthFilterChange(value)}
        >
          {value === "all" ? "All" : value === 0 ? "0" : `≤${value}`}
        </button>
      ))}
      {deployments.length > 0 ? (
        <>
          <span className="subagent-lineage-filter-label">Deployment</span>
          {deployments.map((did) => (
            <button
              key={did}
              type="button"
              className="subagent-lineage-chip"
              aria-pressed={deploymentFilter.has(did)}
              title={did}
              onClick={() => onDeploymentToggle(did)}
            >
              {shortenDid(did)}
            </button>
          ))}
        </>
      ) : null}
      <label className="subagent-lineage-live-toggle">
        <input
          type="checkbox"
          checked={liveOnly}
          onChange={(event) => onLiveOnlyChange(event.currentTarget.checked)}
        />
        Live only
      </label>
    </div>
  );
}
