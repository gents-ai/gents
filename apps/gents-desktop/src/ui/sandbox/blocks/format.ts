/* Values as the SDK's blocks print them. A column of numbers and a chart's
   axis go through the same formatter, so one page never shows the same
   figure two ways. */

/** a field name as a label: `created_at` reads "Created at" */
export function humanize(key: string): string {
  const words = key.replace(/[_-]+/g, " ").trim();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** a finite number, or null for anything a chart cannot plot */
export function numeric(value: unknown): number | null {
  const n =
    typeof value === "number"
      ? value
      : typeof value === "string" && value.trim() !== ""
        ? Number(value)
        : Number.NaN;
  return Number.isFinite(n) ? n : null;
}

const grouped = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const compact = new Intl.NumberFormat(undefined, {
  notation: "compact",
  maximumFractionDigits: 1,
});

/** 1,284 in full under ten thousand, 12.9K from there */
export function formatNumber(n: number): string {
  return Math.abs(n) >= 10_000 ? compact.format(n) : grouped.format(n);
}

const ISO_DATE = /^\d{4}-\d{2}-\d{2}/;
const DATE_ONLY = /^(\d{4})-(\d{2})-(\d{2})$/;

/* a date with no time is a day where the reader is; Date.parse would read it
   as UTC midnight and label it the day before west of Greenwich */
export function timeOf(value: unknown): number | null {
  if (value instanceof Date) return value.getTime();
  if (typeof value !== "string" || !ISO_DATE.test(value)) return null;
  const day = DATE_ONLY.exec(value);
  const time = day
    ? new Date(Number(day[1]), Number(day[2]) - 1, Number(day[3])).getTime()
    : Date.parse(value);
  return Number.isNaN(time) ? null : time;
}

const dayCell = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "short",
  day: "numeric",
});
const momentCell = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** what a table cell shows for a value with no `render` of its own */
export function formatCell(value: unknown): string {
  if (value === null || value === undefined || value === "") return "—";
  if (typeof value === "number") return formatNumber(value);
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (value instanceof Date) return momentCell.format(value);
  if (typeof value === "object") return JSON.stringify(value);
  const time = timeOf(value);
  if (time !== null) {
    return (DATE_ONLY.test(String(value)) ? dayCell : momentCell).format(
      new Date(time),
    );
  }
  return String(value);
}
