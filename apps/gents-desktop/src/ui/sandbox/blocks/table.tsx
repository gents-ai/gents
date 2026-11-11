/* The data table. The kit's own Table wraps itself in an overflow-x
   container, and a sticky cell sticks to its nearest scrolling ancestor, so
   the region that scrolls has to be the direct parent of a bare <table>.
   Separate borders, because a collapsed border scrolls away from a sticky
   header cell. */
import { useMemo, useState, type ReactNode } from "react";
import { Card, CardHeader, CardTitle } from "@gents/ui/components/card";
import {
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@gents/ui/components/table";
import { cn } from "@gents/ui/lib/utils";

import { formatCell, humanize } from "./format";
import { EmptyState } from "./states";

/* `useQuery` can hand over 10,000 rows, and one DOM row per record stops
   scrolling smoothly long before that; the first rows after sorting are the
   ones a reader can use */
const MAX_DRAWN_ROWS = 500;

export type TableRowData = Record<string, unknown>;

export interface TableColumn {
  /** the row field this column reads and sorts by */
  key: string;
  /** default: the key as words */
  label?: ReactNode;
  /** default: right for a column of numbers, left otherwise */
  align?: "left" | "right";
  /** draw the cell yourself; the sort still reads the raw field */
  render?: (value: unknown, row: TableRowData) => ReactNode;
}

export interface TableProps {
  /** field names, or `{ key, label, align, render }` */
  columns: Array<string | TableColumn>;
  rows: TableRowData[];
  title?: ReactNode;
  /** the field that identifies a row, when there is one */
  rowKey?: string;
  defaultSort?: { key: string; direction?: "asc" | "desc" };
  /** default "Nothing to show" */
  empty?: ReactNode;
  /** the height the table scrolls inside, in px; default 420 */
  height?: number;
}

type Sort = { key: string; direction: "asc" | "desc" };

const absent = (value: unknown) => value === null || value === undefined;

function compare(a: unknown, b: unknown): number {
  if (typeof a === "number" && typeof b === "number") return a - b;
  return String(a).localeCompare(String(b), undefined, {
    numeric: true,
    sensitivity: "base",
  });
}

/* a row with nothing to sort by goes last in either direction */
function sorted(rows: TableRowData[], sort: Sort | null): TableRowData[] {
  if (!sort) return rows;
  const order = sort.direction === "asc" ? 1 : -1;
  return [...rows].sort((left, right) => {
    const a = left[sort.key];
    const b = right[sort.key];
    if (absent(a) || absent(b)) return Number(absent(a)) - Number(absent(b));
    return compare(a, b) * order;
  });
}

function SortButton({
  label,
  direction,
  onClick,
}: {
  label: ReactNode;
  direction: "asc" | "desc" | null;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="inline-flex items-center gap-1 hover:text-foreground"
    >
      {label}
      {direction && <span aria-hidden>{direction === "asc" ? "↑" : "↓"}</span>}
    </button>
  );
}

export function Table({
  columns,
  rows,
  title,
  rowKey,
  defaultSort,
  empty,
  height = 420,
}: TableProps) {
  const [sort, setSort] = useState<Sort | null>(
    defaultSort
      ? { key: defaultSort.key, direction: defaultSort.direction ?? "asc" }
      : null,
  );
  const shown = useMemo(() => sorted(rows, sort), [rows, sort]);
  const resolved = columns.map((column) => {
    const c = typeof column === "string" ? { key: column } : column;
    const values = rows.map((row) => row[c.key]).filter((value) => !absent(value));
    return {
      ...c,
      /* a column that reads no field, such as a row's actions, has no order */
      sortable: values.length > 0,
      align:
        c.align ??
        (values.length > 0 && values.every((value) => typeof value === "number")
          ? "right"
          : "left"),
    };
  });
  const toggle = (key: string) =>
    setSort((current) => ({
      key,
      direction: current?.key === key && current.direction === "asc" ? "desc" : "asc",
    }));
  return (
    <Card size="sm" data-slot="data-table" className="gap-0 py-0">
      {title && (
        <CardHeader className="py-3">
          <CardTitle>{title}</CardTitle>
        </CardHeader>
      )}
      {rows.length === 0 ? (
        <EmptyState title={empty} />
      ) : (
        <div
          tabIndex={0}
          className="thin-scrollbar overflow-auto"
          style={{ maxHeight: height }}
        >
          <table className="w-full border-separate border-spacing-0 text-sm tabular-nums">
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                {resolved.map((c) => {
                  const direction = sort?.key === c.key ? sort.direction : null;
                  const label = c.label ?? humanize(c.key);
                  return (
                    <TableHead
                      key={c.key}
                      aria-sort={
                        direction === "asc"
                          ? "ascending"
                          : direction === "desc"
                            ? "descending"
                            : undefined
                      }
                      className={cn(
                        "sticky top-0 z-10 border-b border-border bg-card px-4 text-xs text-muted-foreground",
                        c.align === "right" && "text-right",
                      )}
                    >
                      {c.sortable ? (
                        <SortButton
                          label={label}
                          direction={direction}
                          onClick={() => toggle(c.key)}
                        />
                      ) : (
                        label
                      )}
                    </TableHead>
                  );
                })}
              </TableRow>
            </TableHeader>
            <TableBody>
              {shown.slice(0, MAX_DRAWN_ROWS).map((row, index) => (
                <TableRow key={rowKey ? String(row[rowKey] ?? index) : index}>
                  {resolved.map((c) => (
                    <TableCell
                      key={c.key}
                      className={cn(
                        "border-b border-border px-4 py-2 whitespace-normal",
                        c.align === "right" && "text-right",
                      )}
                    >
                      {c.render ? c.render(row[c.key], row) : formatCell(row[c.key])}
                    </TableCell>
                  ))}
                </TableRow>
              ))}
            </TableBody>
          </table>
        </div>
      )}
      {rows.length > MAX_DRAWN_ROWS && (
        <p className="border-t border-border px-4 py-2 text-xs text-muted-foreground">
          Showing the first {MAX_DRAWN_ROWS} of {rows.length} rows
        </p>
      )}
    </Card>
  );
}
