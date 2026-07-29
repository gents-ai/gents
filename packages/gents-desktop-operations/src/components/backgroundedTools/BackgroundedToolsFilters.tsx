import { shortId } from "../../shortId.js";
import type { DerivedState } from "./derivedState.js";

const STATE_LABELS: Record<DerivedState, string> = {
  running: "Running",
  background: "Background",
  stuck: "Stuck",
  cancelPending: "CancelPending",
  "deadline+": "Past deadline",
};

export type BackgroundedToolsFiltersProps = {
  awaitFilters: Set<string>;
  awaitOptions: string[];
  hideHealthy: boolean;
  parentFilter: string;
  parents: string[];
  stateFilters: Set<DerivedState>;
  stateOptions: DerivedState[];
  onAwaitFilterToggle: (mode: string) => void;
  onHideHealthyChange: (hide: boolean) => void;
  onParentFilterChange: (requestId: string) => void;
  onStateFilterToggle: (state: DerivedState) => void;
};

export function BackgroundedToolsFilters({
  awaitFilters,
  awaitOptions,
  hideHealthy,
  parentFilter,
  parents,
  stateFilters,
  stateOptions,
  onAwaitFilterToggle,
  onHideHealthyChange,
  onParentFilterChange,
  onStateFilterToggle,
}: BackgroundedToolsFiltersProps) {
  return (
    <>
      <div className="chip-row" role="group" aria-label="Filter by parent">
        <span className="chip-label">Parent</span>
        <button
          type="button"
          className={`chip ${parentFilter === "all" ? "is-active" : ""}`}
          aria-pressed={parentFilter === "all"}
          onClick={() => onParentFilterChange("all")}
        >
          All
        </button>
        {parents.map((requestId) => (
          <button
            key={requestId}
            type="button"
            className={`chip ${parentFilter === requestId ? "is-active" : ""}`}
            aria-pressed={parentFilter === requestId}
            onClick={() => onParentFilterChange(requestId)}
            title={requestId}
          >
            {shortId(requestId)}
          </button>
        ))}
      </div>
      {stateOptions.length > 0 ? (
        <div className="chip-row" role="group" aria-label="Filter by state">
          <span className="chip-label">State</span>
          {stateOptions.map((state) => (
            <button
              key={state}
              type="button"
              className={`chip ${stateFilters.has(state) ? "is-active" : ""}`}
              aria-pressed={stateFilters.has(state)}
              onClick={() => onStateFilterToggle(state)}
            >
              {STATE_LABELS[state]}
            </button>
          ))}
        </div>
      ) : null}
      {awaitOptions.length > 0 ? (
        <div
          className="chip-row"
          role="group"
          aria-label="Filter by await mode"
        >
          <span className="chip-label">Await</span>
          {awaitOptions.map((mode) => (
            <button
              key={mode}
              type="button"
              className={`chip ${awaitFilters.has(mode) ? "is-active" : ""}`}
              aria-pressed={awaitFilters.has(mode)}
              onClick={() => onAwaitFilterToggle(mode)}
            >
              {mode}
            </button>
          ))}
        </div>
      ) : null}
      <div className="chip-row">
        <span className="chip-label">Threshold</span>
        <label className="toggle">
          <input
            type="checkbox"
            checked={hideHealthy}
            onChange={(event) =>
              onHideHealthyChange(event.currentTarget.checked)
            }
          />
          Show only stuck / cancel-pending / past deadline
        </label>
      </div>
    </>
  );
}
