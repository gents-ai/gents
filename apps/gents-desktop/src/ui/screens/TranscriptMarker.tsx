import type { ReactNode } from "react";

/** A rule across the transcript naming what sits between rows: a new day. */
export function TranscriptMarker({
  label,
  testId,
}: {
  label: ReactNode;
  testId?: string;
}) {
  return (
    <div
      role="separator"
      data-testid={testId}
      className="flex items-center gap-3 py-1 text-xs text-muted-foreground"
    >
      <span aria-hidden className="h-px flex-1 bg-border/70" />
      <span className="min-w-0 shrink truncate font-medium text-foreground/80">
        {label}
      </span>
      <span aria-hidden className="h-px flex-1 bg-border/70" />
    </div>
  );
}

const dayStart = (date: Date) =>
  new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();

/** the local day a timestamp falls on, or null when it can't be read */
export function dayOf(timestamp: string | null | undefined): number | null {
  if (!timestamp) return null;
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime()) ? null : dayStart(date);
}

export function dayLabel(day: number, now = new Date()) {
  const daysAgo = Math.round((dayStart(now) - day) / 86_400_000);
  if (daysAgo === 0) return "Today";
  if (daysAgo === 1) return "Yesterday";
  const date = new Date(day);
  return date.toLocaleDateString(undefined, {
    weekday: "long",
    day: "numeric",
    month: "long",
    ...(date.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  });
}

export function DayMarker({ day }: { day: number }) {
  return <TranscriptMarker label={dayLabel(day)} testId="day-marker" />;
}

/* the rows that open a day: the first dated row after one from an earlier
   day, or the first dated row drawn */
export function dayOpenings<T>(
  rows: T[],
  keyOf: (row: T) => string,
  timestampOf: (row: T) => string | null | undefined,
) {
  const opens = new Map<string, number>();
  let last: number | null = null;
  for (const row of rows) {
    const day = dayOf(timestampOf(row));
    if (day === null || day === last) continue;
    opens.set(keyOf(row), day);
    last = day;
  }
  return opens;
}
