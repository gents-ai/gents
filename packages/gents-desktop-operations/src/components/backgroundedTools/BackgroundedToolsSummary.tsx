import type { RuntimeView } from "@source-inc/gents-desktop-client";

export type BackgroundedToolsSummaryProps = {
  filteredCount: number;
  projectedCount: number;
  runtime?: RuntimeView | null;
};

export function BackgroundedToolsSummary({
  filteredCount,
  projectedCount,
  runtime,
}: BackgroundedToolsSummaryProps) {
  return (
    <div className="panel-summary">
      <div className="live-count" data-testid="ops-live-count">
        <em>{projectedCount}</em> backgrounded
        {filteredCount !== projectedCount ? (
          <span className="root"> · {filteredCount} shown</span>
        ) : null}
      </div>
      {runtime?.behaviorExecutorCapacity != null ? (
        <div
          className="live-count"
          data-testid="ops-slot-capacity"
          title="Behavior executor slots reported by the agent's runtime document"
        >
          capacity <em>{runtime.behaviorExecutorCapacity}</em>
          {runtime.behaviorExecutorQueueDepth != null &&
          runtime.behaviorExecutorQueueDepth > 0 ? (
            <span className="root">
              {" "}
              · {runtime.behaviorExecutorQueueDepth} queued
            </span>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
