import { shortId } from "../../shortId.js";
import { formatAge } from "./derivedState.js";
import type {
  BackgroundedToolsSortDir,
  BackgroundedToolsSortKey,
  ProjectedBackgroundedTool,
} from "./useBackgroundedToolsModel.js";

const COLUMNS: BackgroundedToolsSortKey[] = [
  "toolName",
  "ageMs",
  "requestId",
  "awaitMode",
  "derivedState",
  "processLabel",
];
const COLUMN_LABELS: Record<BackgroundedToolsSortKey, string> = {
  toolName: "Tool",
  ageMs: "Age",
  requestId: "Parent",
  awaitMode: "Await",
  derivedState: "Status",
  processLabel: "Process",
};

export type BackgroundedToolsTableProps = {
  isLoading: boolean;
  rows: ProjectedBackgroundedTool[];
  sortDir: BackgroundedToolsSortDir;
  sortKey: BackgroundedToolsSortKey;
  onActivateLineage?: () => void;
  onInterruptParent?: (requestId: string) => void;
  onOpenLineage?: (requestId: string) => void;
  onSort: (key: BackgroundedToolsSortKey) => void;
};

export function BackgroundedToolsTable({
  isLoading,
  rows,
  sortDir,
  sortKey,
  onActivateLineage,
  onInterruptParent,
  onOpenLineage,
  onSort,
}: BackgroundedToolsTableProps) {
  return (
    <div className="tools-table-wrap">
      <table className="tools" role="grid">
        <thead>
          <tr>
            {COLUMNS.map((key) => (
              <th
                key={key}
                scope="col"
                tabIndex={0}
                aria-sort={sortKey === key ? sortDir : "none"}
                onClick={() => onSort(key)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    onSort(key);
                  }
                }}
              >
                {COLUMN_LABELS[key]}
              </th>
            ))}
            <th scope="col" aria-label="Row actions" />
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 && !isLoading ? <EmptyRow /> : null}
          {rows.map((row) => (
            <BackgroundedToolRow
              key={row.toolCallId}
              row={row}
              onActivateLineage={onActivateLineage}
              onInterruptParent={onInterruptParent}
              onOpenLineage={onOpenLineage}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function EmptyRow() {
  return (
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
  );
}

function BackgroundedToolRow({
  row,
  onActivateLineage,
  onInterruptParent,
  onOpenLineage,
}: {
  row: ProjectedBackgroundedTool;
  onActivateLineage?: () => void;
  onInterruptParent?: (requestId: string) => void;
  onOpenLineage?: (requestId: string) => void;
}) {
  const isWarning = ["stuck", "cancelPending", "deadline+"].includes(
    row.derivedState,
  );
  return (
    <tr tabIndex={0} className={isWarning ? "row-stuck" : ""}>
      <td className="cell-tool">{row.toolName}</td>
      <td className="cell-age">{formatAge(row.ageMs)}</td>
      <td className="cell-parent" title={row.requestId}>
        {shortId(row.requestId)}
      </td>
      <td>
        <span className="pill pill-await" data-mode={row.awaitMode ?? ""}>
          {row.awaitMode ?? "—"}
        </span>
      </td>
      <td>
        <span className="pill pill-status" data-state={row.derivedState}>
          {row.derivedState === "stuck" || row.derivedState === "cancelPending"
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
            disabled={!onOpenLineage && !onActivateLineage}
            onClick={(event) => {
              event.stopPropagation();
              onOpenLineage?.(row.requestId);
              onActivateLineage?.();
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
            onClick={(event) => {
              event.stopPropagation();
              onInterruptParent?.(row.requestId);
            }}
          >
            Interrupt
          </button>
        </div>
      </td>
    </tr>
  );
}
