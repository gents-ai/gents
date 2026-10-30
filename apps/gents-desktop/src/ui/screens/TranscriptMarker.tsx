import { useId, useState, type ReactNode } from "react";
import { ChevronRight, FoldVertical } from "lucide-react";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import { compactTokens } from "@/lib/tokens";
import { cn } from "@gents/ui/lib/utils";
import {
  checkpointSections,
  isLeadingSection,
  sectionLabel,
  type CheckpointSection,
} from "@/lib/checkpoint";
import { Markdown } from "./Markdown";

/** A rule across the transcript naming something that happened between
    messages: a new day, a compaction. A marker with something to open is
    not a separator, whose content is hidden from assistive technology;
    its whole middle, not the rule, is the button that opens it. */
export function TranscriptMarker({
  icon,
  label,
  detail,
  time,
  testId,
  toggle,
}: {
  icon?: ReactNode;
  label: ReactNode;
  detail?: ReactNode;
  time?: string | null;
  testId?: string;
  toggle?: { open: boolean; controls: string; onToggle: () => void };
}) {
  const content = (
    <>
      {icon && (
        <span aria-hidden className="shrink-0 [&_svg]:size-3.5">
          {icon}
        </span>
      )}
      <span className="truncate font-medium text-foreground/80 group-hover:text-foreground">
        {label}
      </span>
      {toggle && (
        <ChevronRight
          aria-hidden
          className={cn(
            "size-3 shrink-0 transition-transform",
            toggle.open && "rotate-90",
          )}
        />
      )}
      {detail && <span className="truncate">· {detail}</span>}
      {time && (
        <time dateTime={time} title={fullTime(time)} className="shrink-0">
          · {clockTime(time)}
        </time>
      )}
    </>
  );
  return (
    <div
      role={toggle ? undefined : "separator"}
      data-testid={testId}
      className="flex items-center gap-3 py-1 text-xs text-muted-foreground"
    >
      <span aria-hidden className="h-px flex-1 bg-border/70" />
      {toggle ? (
        <button
          type="button"
          aria-expanded={toggle.open}
          aria-controls={toggle.controls}
          title={toggle.open ? "Hide the summary" : "Show what the agent kept"}
          onClick={toggle.onToggle}
          className="group -my-0.5 flex min-w-0 shrink cursor-pointer items-center gap-1.5 rounded-md px-2 py-0.5 hover:bg-muted hover:text-foreground/80 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
        >
          {content}
        </button>
      ) : (
        <span className="flex min-w-0 shrink items-center gap-1.5">{content}</span>
      )}
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

export const fullTime = (timestamp: string) =>
  new Date(timestamp).toLocaleString(undefined, {
    dateStyle: "full",
    timeStyle: "short",
  });

/** a time as a marker gives it, with its date when that isn't today, for
    places without day markers around them; null when it can't be read */
export function shortTime(timestamp: string, now = new Date()): string | null {
  const day = dayOf(timestamp);
  if (day === null) return null;
  if (day === dayStart(now)) return clockTime(timestamp);
  const date = new Date(timestamp);
  return `${date.toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
    ...(date.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  })}, ${clockTime(timestamp)}`;
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

export function CompactionMarker({
  item,
}: {
  item: Extract<RenderedTimelineItem, { kind: "sessionCompaction" }>;
}) {
  const { originalTokens: before, compactedTokens: after } = item;
  const saved =
    before != null && after != null && before > 0
      ? Math.round(((before - after) / before) * 100)
      : null;
  const [open, setOpen] = useState(false);
  const panel = useId();
  const summary = item.summary?.trim() || null;
  const filesRead = item.filesRead ?? [];
  const filesModified = item.filesModified ?? [];
  /* the runtime counts what the model was sent, not the transcript's
     messages, so the marker names no count */
  const title = "Compacted";
  return (
    <div data-testid="session-compaction-event" data-compaction-key={item.itemKey}>
      <TranscriptMarker
        icon={<FoldVertical />}
        label={title}
        toggle={
          summary === null
            ? undefined
            : { open, controls: panel, onToggle: () => setOpen((value) => !value) }
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
          className="mt-1 mb-2 rounded-md border border-border/70 bg-muted/30 px-4 py-3 text-[13px] leading-[1.6]"
        >
          <CheckpointRows
            summary={summary}
            filesRead={filesRead}
            filesModified={filesModified}
          />
        </div>
      )}
    </div>
  );
}

/* the summary a row per section, a short label beside its text; the goal,
   next actions, uncertainties and blockers show first */
function CheckpointRows({
  summary,
  filesRead,
  filesModified,
}: {
  summary: string;
  filesRead: string[];
  filesModified: string[];
}) {
  const [all, setAll] = useState(false);
  const sections = checkpointSections(summary);
  if (!sections) {
    return (
      <>
        <div className="prose-app text-[13px]">
          <Markdown>{summary}</Markdown>
        </div>
        {(filesRead.length > 0 || filesModified.length > 0) && (
          <dl className="mt-2 grid grid-cols-[5rem_minmax(0,1fr)] gap-x-4 gap-y-3">
            <FileRow label="Files read" files={filesRead} />
            <FileRow label="Files modified" files={filesModified} />
          </dl>
        )}
      </>
    );
  }
  /* the entry's own file lists stand in for the ones written into the text */
  const fileTitles = new Set([
    ...(filesRead.length ? ["Files read"] : []),
    ...(filesModified.length ? ["Files modified"] : []),
  ]);
  const shown = sections.filter((section) => !fileTitles.has(section.title));
  const leading = shown.filter(isLeadingSection);
  const first = leading.length ? leading : shown;
  const rest = shown.length - first.length + fileTitles.size;
  return (
    <>
      <dl className="grid grid-cols-[5rem_minmax(0,1fr)] gap-x-4 gap-y-3">
        {(all ? shown : first).map((section) => (
          <SectionRow key={section.title} section={section} />
        ))}
        {all && (
          <>
            <FileRow label="Files read" files={filesRead} />
            <FileRow label="Files modified" files={filesModified} />
          </>
        )}
      </dl>
      {rest > 0 && (
        <button
          type="button"
          aria-expanded={all}
          onClick={() => setAll((value) => !value)}
          className="mt-1.5 cursor-pointer rounded-sm text-xs text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
        >
          {all ? "Show less" : `Show ${rest} more`}
        </button>
      )}
    </>
  );
}

function SectionRow({ section }: { section: CheckpointSection }) {
  const List = section.ordered ? "ol" : "ul";
  return (
    <>
      <dt className="text-xs leading-[1.3rem] text-muted-foreground">
        {sectionLabel(section.title)}
      </dt>
      <dd className="min-w-0 max-w-[72ch] text-foreground/90">
        {section.text && <p>{inlineCode(section.text)}</p>}
        {section.items.length > 0 && (
          <List
            className={cn(
              "space-y-1 pl-4",
              section.ordered ? "list-decimal" : "list-disc",
              "marker:text-muted-foreground",
            )}
          >
            {section.items.map((item, index) => (
              <li key={index}>{inlineCode(item)}</li>
            ))}
          </List>
        )}
      </dd>
    </>
  );
}

function FileRow({ label, files }: { label: string; files: string[] }) {
  if (!files.length) return null;
  return (
    <>
      <dt className="pt-0.5 text-xs text-muted-foreground">{sectionLabel(label)}</dt>
      <dd className="min-w-0 max-w-[72ch] text-foreground/90">
        <ul aria-label={label} className="flex flex-wrap gap-1">
          {files.map((file) => (
            <li
              key={file}
              className="max-w-full truncate rounded border border-border/70 px-1.5 py-px font-mono text-[11px]"
              title={file}
            >
              {file}
            </li>
          ))}
        </ul>
      </dd>
    </>
  );
}

/* backtick spans as code; the rest of the checkpoint's Markdown is plain */
function inlineCode(text: string) {
  return text.split(/`([^`]+)`/).map((part, index) =>
    index % 2 ? (
      <code key={index} className="rounded bg-muted px-1 font-mono text-[12px]">
        {part}
      </code>
    ) : (
      part
    ),
  );
}
