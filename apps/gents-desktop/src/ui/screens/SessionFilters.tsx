/* The session filters as one quiet row at the right of the heading: an icon
   per axis, each opening its own short menu. The state glyphs are the ones
   the rows carry, so the strip reads as the list's legend as well as its
   filter. Every option stays listed whether or not it has anything right
   now, with the count it would leave behind faceted against the other two
   axes, so narrowing never hides the way back. */
import {
  Activity,
  ChevronDown,
  CircleX,
  CornerDownRight,
  Play,
  User,
  X,
} from "lucide-react";
import { useEffect, useState } from "react";
import type { ComponentType, ReactNode } from "react";
import type { DeploymentView, SessionSummary } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import {
  Combobox,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  ComboboxTrigger,
} from "@gents/ui/components/combobox";
import { cn } from "@gents/ui/lib/utils";
import { isLive } from "@/lib/live";
import { BehaviorAvatar } from "./parts";

export type SessionState = "live" | "held" | "failed";
export type SessionSource = "person" | "task" | "session";

export type SessionFilter = {
  states: SessionState[];
  sources: SessionSource[];
  behaviors: string[];
};

export const emptyFilter: SessionFilter = { states: [], sources: [], behaviors: [] };
export const hasFilter = (f: SessionFilter) =>
  f.states.length > 0 || f.sources.length > 0 || f.behaviors.length > 0;

const KEY = "gents-session-filter";
const strings = (v: unknown) =>
  Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];

/* the narrowing outlives the visit: a person who works from "needs you"
   comes back to it. The trigger takes full ink and the row carries a
   clear while anything is set, so a filter is never quietly on. */
export function useSessionFilter() {
  const [filter, setFilter] = useState<SessionFilter>(() => {
    try {
      const raw = localStorage.getItem(KEY);
      if (!raw) return emptyFilter;
      const parsed = JSON.parse(raw) as Record<string, unknown>;
      return {
        states: strings(parsed.states) as SessionState[],
        sources: strings(parsed.sources) as SessionSource[],
        behaviors: strings(parsed.behaviors),
      };
    } catch {
      return emptyFilter;
    }
  });
  useEffect(() => {
    try {
      if (hasFilter(filter)) localStorage.setItem(KEY, JSON.stringify(filter));
      else localStorage.removeItem(KEY);
    } catch {
      /* storage unavailable */
    }
  }, [filter]);
  return [filter, setFilter] as const;
}

const matchesState = (c: SessionSummary, state: SessionState, held: Set<string>) =>
  state === "live"
    ? isLive(c.turnState)
    : state === "held"
      ? held.has(c.sessionId)
      : c.turnState === "failed";

/* One option for automation, not two. Every run a trigger fires also names
   the task it fired, so a task and a trigger were the same sessions under
   two names, and the counts added up to more than the list held. Splitting
   them properly — a trigger fired it, or someone invoked the task by hand —
   asks a question about configuration rather than about the session, and
   leaves an option that is almost always empty. How it fired is on the
   session itself, where there is room to say it. */
const matchesSource = (c: SessionSummary, source: SessionSource) =>
  source === "task"
    ? Boolean(c.taskId || c.triggerId)
    : source === "session"
      ? Boolean(c.provenance?.parent_request_doc_id)
      : !(c.taskId || c.triggerId || c.provenance?.parent_request_doc_id);

/* an axis passes when nothing on it is picked, or when one picked value
   matches: within an axis the options are a union, across axes they meet */
const passes = (c: SessionSummary, f: SessionFilter, held: Set<string>) =>
  (f.states.length === 0 || f.states.some((s) => matchesState(c, s, held))) &&
  (f.sources.length === 0 || f.sources.some((s) => matchesSource(c, s))) &&
  (f.behaviors.length === 0 || f.behaviors.includes(c.behaviorId ?? ""));

export const filterSessions = (
  sessions: SessionSummary[],
  f: SessionFilter,
  held: Set<string>,
  query: string | null,
) =>
  sessions.filter(
    (c) =>
      passes(c, f, held) &&
      (!query || (c.title ?? "").toLowerCase().includes(query.toLowerCase())),
  );

/* the held mark the rows use: a filled brand dot, not a Lucide glyph */
function HeldDot({ className }: { className?: string }) {
  return <span className={cn("size-2 rounded-full bg-brand", className)} />;
}

type Option<V extends string> = {
  value: V;
  label: string;
  icon: ComponentType<{ className?: string }>;
  tint?: string;
};

const STATES: Option<SessionState>[] = [
  { value: "live", label: "Live", icon: Activity },
  { value: "held", label: "Needs you", icon: HeldDot },
  { value: "failed", label: "Failed", icon: CircleX, tint: "text-destructive" },
];

const SOURCES: Option<SessionSource>[] = [
  { value: "person", label: "A person", icon: User },
  { value: "task", label: "A task", icon: Play },
  { value: "session", label: "Another session", icon: CornerDownRight },
];

/* One axis: the icon of what is picked, or the axis's own icon when it is
   open to everything, and the menu of its options with their counts. */
function Axis<V extends string>({
  label,
  icon: AxisIcon,
  options,
  counts,
  value,
  onChange,
}: {
  label: string;
  icon: ComponentType<{ className?: string }>;
  options: Option<V>[];
  counts: Record<string, number>;
  value: V[];
  onChange: (next: V[]) => void;
}) {
  const picked = options.filter((o) => value.includes(o.value));
  const toggle = (v: V) =>
    onChange(value.includes(v) ? value.filter((x) => x !== v) : [...value, v]);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="quiet"
            size="sm"
            aria-label={label}
            className={cn("gap-1.5 px-2", picked.length > 0 && "text-foreground")}
          />
        }
      >
        {/* one steady mark for the axis: the picked options say what they
            are in the label and the menu, so the trigger does not need to
            restate them and shift width as they change */}
        <AxisIcon className="size-4" />
        {/* on a phone the row has no width to name its axes: the marks
            carry the meaning and the words come back at sm */}
        <span className="hidden text-xs sm:inline">
          {picked.length === 1 ? picked[0].label : label}
        </span>
        <ChevronDown className="hidden size-3 opacity-50 sm:block" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-52">
        <DropdownMenuGroup>
          {options.map(({ value: v, label: text, icon: Icon, tint }) => (
            <DropdownMenuCheckboxItem
              key={v}
              checked={value.includes(v)}
              /* an option with nothing behind it keeps its place and its
                 zero: a filter that vanishes as you narrow loses the way
                 back, and hides why it went */
              disabled={(counts[v] ?? 0) === 0 && !value.includes(v)}
              onCheckedChange={() => toggle(v)}
              closeOnClick={false}
            >
              <Icon className={cn("size-4 shrink-0", tint)} />
              <span className="min-w-0 flex-1 truncate">{text}</span>
              <span className="text-xs tabular-nums text-muted-foreground">
                {counts[v] ?? 0}
              </span>
            </DropdownMenuCheckboxItem>
          ))}
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/* Behavior is the one axis with no ceiling, so it folds into a searchable
   control rather than a menu: the trigger wears the marks of what is
   picked, so the row stays one line however many behaviors there are. */
function BehaviorAxis({
  behaviors,
  value,
  onChange,
}: {
  behaviors: { id: string; name: string; count: number }[];
  value: string[];
  onChange: (next: string[]) => void;
}) {
  const byId = new Map(behaviors.map((b) => [b.id, b]));
  const picked = behaviors.filter((b) => value.includes(b.id));
  return (
    <Combobox
      multiple
      items={behaviors.map((b) => b.id)}
      itemToStringLabel={(id: string) => byId.get(id)?.name ?? id}
      value={value}
      onValueChange={(next: string[]) => onChange(next)}
    >
      <ComboboxTrigger
        render={
          <Button
            variant="quiet"
            size="sm"
            aria-label="Behavior"
            /* the two dropdown axes hide their chevron below sm, where the
               marks carry the row on their own; this trigger's chevron is
               drawn by the kit, so it is hidden from here to match rather
               than leaving one axis wider than its neighbors */
            className={cn(
              "gap-1.5 px-2 [&>svg:last-of-type]:hidden sm:[&>svg:last-of-type]:block",
              picked.length > 0 && "text-foreground",
            )}
          />
        }
      >
        {picked.length > 0 ? (
          <span className="flex -space-x-1.5">
            {picked.slice(0, 3).map((b) => (
              <BehaviorAvatar
                key={b.id}
                name={b.name}
                behaviorId={b.id}
                className="size-5 text-[9px] ring-1 ring-background"
              />
            ))}
          </span>
        ) : (
          <Play className="size-4" />
        )}
        <span className="hidden text-xs sm:inline">
          {picked.length === 1 ? picked[0].name : "Behavior"}
        </span>
      </ComboboxTrigger>
      <ComboboxContent className="w-60" aria-label="Filter by behavior">
        <ComboboxInput placeholder="Find a behavior" showTrigger={false} />
        <ComboboxEmpty>No behavior by that name.</ComboboxEmpty>
        <ComboboxList>
          {(id: string) => {
            const b = byId.get(id);
            if (!b) return null;
            return (
              <ComboboxItem
                key={id}
                value={id}
                disabled={b.count === 0 && !value.includes(id)}
              >
                <BehaviorAvatar
                  name={b.name}
                  behaviorId={b.id}
                  className="size-5 text-[9px]"
                />
                <span className="min-w-0 flex-1 truncate">{b.name}</span>
                <span className="text-xs tabular-nums text-muted-foreground">
                  {b.count}
                </span>
              </ComboboxItem>
            );
          }}
        </ComboboxList>
      </ComboboxContent>
    </Combobox>
  );
}

export function SessionFilters({
  sessions,
  deployment,
  held,
  value,
  onChange,
}: {
  sessions: SessionSummary[];
  deployment: DeploymentView | null;
  held: Set<string>;
  value: SessionFilter;
  onChange: (next: SessionFilter) => void;
}): ReactNode {
  /* a count says what its option would leave: its own axis is dropped from
     the filter, so the numbers answer "and how many of those" as you narrow */
  const countFor = (axis: keyof SessionFilter, test: (c: SessionSummary) => boolean) =>
    sessions.filter((c) => passes(c, { ...value, [axis]: [] }, held) && test(c)).length;

  const stateCounts = Object.fromEntries(
    STATES.map((s) => [
      s.value,
      countFor("states", (c) => matchesState(c, s.value, held)),
    ]),
  );
  const sourceCounts = Object.fromEntries(
    SOURCES.map((s) => [
      s.value,
      countFor("sources", (c) => matchesSource(c, s.value)),
    ]),
  );
  const behaviors = (deployment?.behaviors ?? []).map((b) => ({
    id: b.behaviorId,
    name: b.displayName,
    count: countFor("behaviors", (c) => c.behaviorId === b.behaviorId),
  }));

  if (sessions.length === 0) return null;

  return (
    <div
      aria-label="Session filters"
      className="flex min-w-0 shrink items-center gap-0.5 text-muted-foreground"
    >
      <Axis
        label="State"
        icon={Activity}
        options={STATES}
        counts={stateCounts}
        value={value.states}
        onChange={(states) => onChange({ ...value, states })}
      />
      <Axis
        label="Started by"
        icon={User}
        options={SOURCES}
        counts={sourceCounts}
        value={value.sources}
        onChange={(sources) => onChange({ ...value, sources })}
      />
      {behaviors.length > 0 && (
        <BehaviorAxis
          behaviors={behaviors}
          value={value.behaviors}
          onChange={(next) => onChange({ ...value, behaviors: next })}
        />
      )}
      {hasFilter(value) && (
        <Button
          variant="quiet"
          size="icon-sm"
          aria-label="Clear filters"
          title="Clear filters"
          className="shrink-0"
          onClick={() => onChange(emptyFilter)}
        >
          <X />
        </Button>
      )}
    </div>
  );
}
