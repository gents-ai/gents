import type { FilterId, VisualState } from "./mcpHealthModel";

export type McpHealthSummaryCounts = Record<VisualState, number>;

export type McpHealthFilterCounts = {
  all: number;
  unhealthy: number;
  reconnecting: number;
};

export function McpHealthSummary({ summary }: { summary: McpHealthSummaryCounts }) {
  return (
    <div className="mcp-health-summary" aria-label="Service health summary">
      <SummaryPill color="green" count={summary.healthy} label="healthy" />
      <SummaryPill color="yellow" count={summary.degraded} label="degraded" />
      <SummaryPill color="blue" count={summary.reconnecting} label="reconnecting" />
      <SummaryPill color="red" count={summary.evicted} label="evicted" />
      <SummaryPill color="red" count={summary.stuck} label="stuck" />
      {summary.unknown > 0 ? (
        <SummaryPill color="gray" count={summary.unknown} label="unknown" />
      ) : null}
    </div>
  );
}

export function McpHealthFilters({
  filter,
  counts,
  onFilterChange,
}: {
  filter: FilterId;
  counts: McpHealthFilterCounts;
  onFilterChange: (filter: FilterId) => void;
}) {
  return (
    <div role="group" aria-label="Filter" className="mcp-health-filters">
      <FilterChip
        label="All"
        count={counts.all}
        active={filter === "all"}
        onClick={() => onFilterChange("all")}
      />
      <FilterChip
        label="Unhealthy"
        count={counts.unhealthy}
        active={filter === "unhealthy"}
        onClick={() => onFilterChange("unhealthy")}
      />
      <FilterChip
        label="Reconnecting"
        count={counts.reconnecting}
        active={filter === "reconnecting"}
        onClick={() => onFilterChange("reconnecting")}
      />
    </div>
  );
}

function SummaryPill({
  color,
  count,
  label,
}: {
  color: "green" | "yellow" | "red" | "blue" | "gray";
  count: number;
  label: string;
}) {
  return (
    <span className={`mcp-health-pill mcp-health-pill-${color}`}>
      <span className="mcp-health-pill-dot" aria-hidden />
      <span className="mcp-health-pill-count">{count}</span> {label}
    </span>
  );
}

function FilterChip({
  label,
  count,
  active,
  onClick,
}: {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={active ? "mcp-health-chip is-active" : "mcp-health-chip"}
      aria-pressed={active}
      onClick={onClick}
    >
      {label} <span className="mcp-health-chip-count">{count}</span>
    </button>
  );
}
