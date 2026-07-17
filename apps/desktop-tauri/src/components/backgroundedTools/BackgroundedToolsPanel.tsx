import { useCallback, useContext, useMemo, useState } from "react";

import type {
  DesktopOperationsSnapshotRequest,
  StuckWorkDiagnosticView,
} from "../../lib/types/operations";
import { OperationsRailContext } from "../operations/operationsRailContext";
import {
  correlateProcess,
  derivedState,
  formatAge,
  type DerivedState,
} from "./derivedState";
import { useOperationsSnapshot } from "./useOperationsSnapshot";

type SortKey =
  | "toolName"
  | "ageMs"
  | "requestId"
  | "awaitMode"
  | "derivedState"
  | "processLabel";
type SortDir = "ascending" | "descending";

const STATE_LABELS: Record<string, string> = {
  running: "Running",
  background: "Background",
  stuck: "Stuck",
  cancelPending: "CancelPending",
  "deadline+": "Past deadline",
};

function shortRequestId(requestId: string): string {
  return requestId.length > 14 ? `${requestId.slice(0, 14)}…` : requestId;
}

/** The bridge's diagnosis, in operator language. */
function diagnosticSentence(diag: StuckWorkDiagnosticView): string {
  const tool = diag.toolName ?? "a tool";
  switch (diag.reason) {
    case "expiredProcessing":
      return `Request ${shortRequestId(diag.requestId)} ran past its deadline`;
    case "expiredTool":
      return `${tool} on ${shortRequestId(diag.requestId)} ran past its deadline`;
    case "stuckTool":
      return `${tool} on ${shortRequestId(diag.requestId)} has stopped making progress`;
    case "pendingRemoteCancelAck":
      return `Waiting on a remote node to acknowledge cancelling ${shortRequestId(diag.requestId)}`;
    default:
      return `${shortRequestId(diag.requestId)} needs attention`;
  }
}

export type BackgroundedToolsPanelProps = {
  rootRequestId?: string | null;
  /** Focus the lineage view on this row's parent request. */
  onOpenLineage?: (requestId: string) => void;
  /** Begin the interrupt (preview + cascade dialog) flow for this row's parent request. */
  onInterruptParent?: (requestId: string) => void;
};

export function BackgroundedToolsPanel({
  rootRequestId,
  onOpenLineage,
  onInterruptParent,
}: BackgroundedToolsPanelProps = {}) {
  // Nullable on purpose: the panel also renders outside the operations rail
  // (tests, future standalone surfaces), where tab switching is a no-op.
  const rail = useContext(OperationsRailContext);
  const request: DesktopOperationsSnapshotRequest = useMemo(
    () => ({ rootRequestId: rootRequestId ?? null }),
    [rootRequestId],
  );
  const { snapshot, error, isLoading } = useOperationsSnapshot(request);

  const [stateFilters, setStateFilters] = useState<Set<DerivedState>>(new Set());
  const [awaitFilters, setAwaitFilters] = useState<Set<string>>(new Set());
  const [parentFilter, setParentFilter] = useState<string>("all");
  const [hideHealthy, setHideHealthy] = useState<boolean>(false);
  const [sortKey, setSortKey] = useState<SortKey>("ageMs");
  const [sortDir, setSortDir] = useState<SortDir>("descending");

  const projected = useMemo(() => {
    if (!snapshot) return [];
    const now = Date.now();
    const execs = snapshot.liveness?.activeNativeExecutors ?? [];
    return snapshot.backgroundedTools.map((row) => {
      const proc = correlateProcess(row, execs);
      return {
        ...row,
        derivedState: derivedState(row, now),
        ageMs: row.ageMs ?? 0,
        processLabel: proc.label,
        processTooltip: proc.tooltip,
      };
    });
  }, [snapshot]);

  const filtered = useMemo(() => {
    const rows = projected.filter((r) => {
      if (parentFilter !== "all" && r.requestId !== parentFilter) return false;
      if (stateFilters.size > 0 && !stateFilters.has(r.derivedState)) return false;
      if (
        awaitFilters.size > 0 &&
        (r.awaitMode == null || !awaitFilters.has(r.awaitMode))
      )
        return false;
      if (
        hideHealthy &&
        !["stuck", "cancelPending", "deadline+"].includes(r.derivedState)
      )
        return false;
      return true;
    });
    const dir = sortDir === "ascending" ? 1 : -1;
    return [...rows].sort((a, b) => {
      const av = (a as unknown as Record<string, unknown>)[sortKey];
      const bv = (b as unknown as Record<string, unknown>)[sortKey];
      if (av == null && bv == null) return 0;
      if (av == null) return 1;
      if (bv == null) return -1;
      if (typeof av === "number" && typeof bv === "number") return (av - bv) * dir;
      return String(av).localeCompare(String(bv)) * dir;
    });
  }, [
    projected,
    parentFilter,
    stateFilters,
    awaitFilters,
    hideHealthy,
    sortKey,
    sortDir,
  ]);

  const parents = useMemo(
    () => Array.from(new Set(projected.map((r) => r.requestId))),
    [projected],
  );

  // Chips are derived from the rows actually present (plus any engaged
  // filter, so clearing stays possible) — an offered filter always matches.
  const stateOptions = useMemo(
    () =>
      Array.from(
        new Set([...projected.map((r) => r.derivedState), ...stateFilters]),
      ).sort(),
    [projected, stateFilters],
  );
  const awaitOptions = useMemo(
    () =>
      Array.from(
        new Set([
          ...projected
            .map((r) => r.awaitMode)
            .filter((mode): mode is string => mode != null),
          ...awaitFilters,
        ]),
      ).sort(),
    [projected, awaitFilters],
  );

  const onSort = useCallback(
    (key: SortKey) => {
      if (sortKey === key) {
        setSortDir((d) => (d === "ascending" ? "descending" : "ascending"));
      } else {
        setSortKey(key);
        setSortDir("ascending");
      }
    },
    [sortKey],
  );

  const toggleStateFilter = (s: DerivedState) =>
    setStateFilters((prev) => {
      const next = new Set(prev);
      next.has(s) ? next.delete(s) : next.add(s);
      return next;
    });

  const toggleAwaitFilter = (a: string) =>
    setAwaitFilters((prev) => {
      const next = new Set(prev);
      next.has(a) ? next.delete(a) : next.add(a);
      return next;
    });

  // Full error state only when there is nothing to show — a single failed
  // poll must not replace a live table with an error screen.
  if (error && !snapshot) {
    return (
      <section className="background-tools-panel" aria-label="Background tools">
        <div className="empty-state">
          <span className="glyph" aria-hidden="true">
            ○
          </span>
          Snapshot bridge unavailable: {error}
        </div>
      </section>
    );
  }

  const diagnostics = snapshot?.stuckDiagnostics ?? [];

  return (
    <section className="background-tools-panel" aria-label="Background tools">
      {error ? (
        <div className="muted small" data-testid="ops-stale-note" role="status">
          Live updates interrupted — showing the last snapshot. ({error})
        </div>
      ) : null}
      {diagnostics.length > 0 ? (
        <div className="stuck-diagnostics" data-testid="stuck-diagnostics" role="alert">
          {diagnostics.map((diag, index) => (
            <div
              className={`stuck-diagnostic is-${diag.severity}`}
              key={`${diag.requestId}-${diag.toolCallId ?? index}`}
            >
              <span aria-hidden="true" className="stuck-diagnostic-dot" />
              {diagnosticSentence(diag)}
            </div>
          ))}
        </div>
      ) : null}
      <div className="chip-row" role="group" aria-label="Filter by parent">
        <span className="chip-label">Parent</span>
        <button
          type="button"
          className={`chip ${parentFilter === "all" ? "is-active" : ""}`}
          aria-pressed={parentFilter === "all"}
          onClick={() => setParentFilter("all")}
        >
          All
        </button>
        {parents.map((p) => (
          <button
            key={p}
            type="button"
            className={`chip ${parentFilter === p ? "is-active" : ""}`}
            aria-pressed={parentFilter === p}
            onClick={() => setParentFilter(p)}
          >
            {p}
          </button>
        ))}
      </div>
      {stateOptions.length > 0 ? (
        <div className="chip-row" role="group" aria-label="Filter by state">
          <span className="chip-label">State</span>
          {stateOptions.map((s) => (
            <button
              key={s}
              type="button"
              className={`chip ${stateFilters.has(s) ? "is-active" : ""}`}
              aria-pressed={stateFilters.has(s)}
              onClick={() => toggleStateFilter(s)}
            >
              {STATE_LABELS[s] ?? s}
            </button>
          ))}
        </div>
      ) : null}
      {awaitOptions.length > 0 ? (
        <div className="chip-row" role="group" aria-label="Filter by await mode">
          <span className="chip-label">Await</span>
          {awaitOptions.map((a) => (
            <button
              key={a}
              type="button"
              className={`chip ${awaitFilters.has(a) ? "is-active" : ""}`}
              aria-pressed={awaitFilters.has(a)}
              onClick={() => toggleAwaitFilter(a)}
            >
              {a}
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
            onChange={(e) => setHideHealthy(e.target.checked)}
          />
          Show only stuck / cancel-pending / past deadline
        </label>
      </div>

      <div className="panel-summary">
        <div className="live-count" data-testid="ops-live-count">
          <em>{projected.length}</em> backgrounded
          {filtered.length !== projected.length ? (
            <span className="root"> · {filtered.length} shown</span>
          ) : null}
        </div>
      </div>

      <div className="tools-table-wrap">
        <table className="tools" role="grid">
          <thead>
            <tr>
              {(
                [
                  "toolName",
                  "ageMs",
                  "requestId",
                  "awaitMode",
                  "derivedState",
                  "processLabel",
                ] as SortKey[]
              ).map((key) => (
                <th
                  key={key}
                  scope="col"
                  tabIndex={0}
                  aria-sort={sortKey === key ? sortDir : "none"}
                  onClick={() => onSort(key)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      onSort(key);
                    }
                  }}
                >
                  {key === "toolName"
                    ? "Tool"
                    : key === "ageMs"
                      ? "Age"
                      : key === "requestId"
                        ? "Parent"
                        : key === "awaitMode"
                          ? "Await"
                          : key === "derivedState"
                            ? "Status"
                            : "Process"}
                </th>
              ))}
              <th scope="col" aria-label="Row actions" />
            </tr>
          </thead>
          <tbody>
            {filtered.length === 0 && !isLoading && (
              <tr>
                <td colSpan={7}>
                  <div className="empty-state">
                    <span className="glyph" aria-hidden="true">
                      ○
                    </span>
                    No backgrounded tools.
                  </div>
                </td>
              </tr>
            )}
            {filtered.map((row) => {
              const isWarn = ["stuck", "cancelPending", "deadline+"].includes(
                row.derivedState,
              );
              return (
                <tr
                  key={row.toolCallId}
                  tabIndex={0}
                  className={isWarn ? "row-stuck" : ""}
                >
                  <td className="cell-tool">{row.toolName}</td>
                  <td className="cell-age">{formatAge(row.ageMs ?? 0)}</td>
                  <td className="cell-parent">{row.requestId}</td>
                  <td>
                    <span className="pill pill-await" data-mode={row.awaitMode ?? ""}>
                      {row.awaitMode ?? "—"}
                    </span>
                  </td>
                  <td>
                    <span className="pill pill-status" data-state={row.derivedState}>
                      {row.derivedState === "stuck" ||
                      row.derivedState === "cancelPending"
                        ? "⚠ "
                        : ""}
                      {row.derivedState}
                    </span>
                  </td>
                  <td
                    className={`cell-process ${row.processLabel === "—" ? "is-empty" : ""}`}
                    title={row.processTooltip}
                  >
                    {row.processLabel}
                  </td>
                  <td>
                    <div className="row-actions">
                      <button
                        type="button"
                        data-testid={`bg-tool-lineage-${row.toolCallId}`}
                        aria-label={`Open lineage for ${row.toolName} on ${row.requestId}`}
                        disabled={!onOpenLineage && !rail}
                        onClick={(e) => {
                          e.stopPropagation();
                          onOpenLineage?.(row.requestId);
                          rail?.setActiveTab("lineage");
                        }}
                      >
                        Lineage
                      </button>
                      <button
                        type="button"
                        className="danger"
                        data-testid={`bg-tool-interrupt-${row.toolCallId}`}
                        aria-label={`Interrupt parent request ${row.requestId}`}
                        disabled={!onInterruptParent}
                        onClick={(e) => {
                          e.stopPropagation();
                          onInterruptParent?.(row.requestId);
                        }}
                      >
                        Interrupt
                      </button>
                    </div>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </section>
  );
}
