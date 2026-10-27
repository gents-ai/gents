import { useId, useState, type ReactNode } from "react";
import { ChevronRight, FoldVertical } from "lucide-react";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import { compactTokens } from "@/lib/tokens";
import { cn } from "@gents/ui/lib/utils";
import { Markdown } from "./Markdown";

/** A rule across the transcript naming something that happened between
    messages: a new day, a compaction. A marker with something to open is
    not a separator, whose content is hidden from assistive technology. */
export function TranscriptMarker({
  icon,
  label,
  detail,
  time,
  testId,
  interactive = false,
}: {
  icon?: ReactNode;
  label: ReactNode;
  detail?: ReactNode;
  time?: string | null;
  testId?: string;
  interactive?: boolean;
}) {
  return (
    <div
      role={interactive ? undefined : "separator"}
      data-testid={testId}
      className="flex items-center gap-3 py-1 text-xs text-muted-foreground"
    >
      <span aria-hidden className="h-px flex-1 bg-border/70" />
      <span className="flex min-w-0 shrink items-center gap-1.5">
        {icon && (
          <span aria-hidden className="shrink-0 [&_svg]:size-3.5">
            {icon}
          </span>
        )}
        <span className="truncate font-medium text-foreground/80">{label}</span>
        {detail && <span className="truncate">· {detail}</span>}
        {time && (
          <time dateTime={time} title={fullTime(time)} className="shrink-0">
            · {clockTime(time)}
          </time>
        )}
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

const clockTime = (timestamp: string) =>
  new Date(timestamp).toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });

const fullTime = (timestamp: string) =>
  new Date(timestamp).toLocaleString(undefined, {
    dateStyle: "full",
    timeStyle: "short",
  });

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

export function CompactionMarker({
  item,
}: {
  item: Extract<RenderedTimelineItem, { kind: "sessionCompaction" }>;
}) {
  const {
    messagesCompacted: messages,
    originalTokens: before,
    compactedTokens: after,
  } = item;
  const saved =
    before != null && after != null && before > 0
      ? Math.round(((before - after) / before) * 100)
      : null;
  const [open, setOpen] = useState(false);
  const panel = useId();
  const summary = item.summary?.trim() || null;
  const filesRead = item.filesRead ?? [];
  const filesModified = item.filesModified ?? [];
  const title =
    messages == null
      ? "Compacted"
      : `Compacted ${messages.toLocaleString()} ${messages === 1 ? "message" : "messages"}`;
  return (
    <div data-testid="session-compaction-event" data-compaction-key={item.itemKey}>
      <TranscriptMarker
        icon={<FoldVertical />}
        interactive={summary !== null}
        label={
          summary === null ? (
            title
          ) : (
            <button
              type="button"
              aria-expanded={open}
              aria-controls={panel}
              title={open ? "Hide the summary" : "Show what the agent kept"}
              onClick={() => setOpen((value) => !value)}
              className="inline-flex cursor-pointer items-center gap-1 rounded-sm hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            >
              {title}
              <ChevronRight
                aria-hidden
                className={cn("size-3 transition-transform", open && "rotate-90")}
              />
            </button>
          )
        }
        detail={
          before != null && after != null
            ? `${compactTokens(before)} → ${compactTokens(after)} tokens${saved != null && saved > 0 ? ` (−${saved}%)` : ""}`
            : null
        }
        time={item.timestamp}
      />
      {summary !== null && open && (
        <div
          id={panel}
          role="region"
          aria-label="Compaction summary"
          data-testid="compaction-summary"
          className="mx-auto mt-1 mb-2 max-w-prose rounded-lg border border-border/70 bg-muted/30 px-4 py-3 text-sm"
        >
          <p className="mb-2 text-xs text-muted-foreground">
            What the agent kept of the compacted messages
          </p>
          <div className="prose-app">
            <Markdown>{summary}</Markdown>
          </div>
          <CompactionFiles label="Files read" files={filesRead} />
          <CompactionFiles label="Files modified" files={filesModified} />
        </div>
      )}
    </div>
  );
}

function CompactionFiles({ label, files }: { label: string; files: string[] }) {
  if (!files.length) return null;
  return (
    <div className="mt-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <ul aria-label={label} className="mt-1 flex flex-wrap gap-1.5">
        {files.map((file) => (
          <li
            key={file}
            className="max-w-full truncate rounded border border-border/70 px-1.5 py-0.5 font-mono text-[11px]"
            title={file}
          >
            {file}
          </li>
        ))}
      </ul>
    </div>
  );
}
