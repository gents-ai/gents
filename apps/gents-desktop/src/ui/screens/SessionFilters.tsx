import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import {
  Activity,
  Circle,
  CircleMinus,
  CircleX,
  Clock,
  CornerDownRight,
  Play,
  User,
} from "lucide-react";
import type { ComponentType, ReactNode } from "react";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import {
  hasFilter,
  isShown,
  matchesSource,
  matchesState,
  passes,
  toggleShown,
  type SessionFilter,
  type SessionSource,
  type SessionState,
} from "@/lib/session-filter";
import { nodeKeyOf } from "../../hooks/fleetStore";
import { useFleet } from "@/hooks/useFleet";
import { agentName } from "./behavior";
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
import { AgentInitials } from "./parts";

export const matchesView = (
  c: SessionSummary,
  f: SessionFilter,
  query: string | null,
) =>
  passes(c, f) &&
  (!query || (c.title ?? "").toLowerCase().includes(query.toLowerCase()));

export type Option<V extends string> = {
  value: V;
  label: string;
  icon: ComponentType<{ className?: string }>;
  tint?: string;
};

export const STATES: Option<SessionState>[] = [
  { value: "working", label: "Working", icon: Activity },
  { value: "waiting", label: "Waiting for node", icon: Clock },
  { value: "failed", label: "Failed", icon: CircleX, tint: "text-destructive" },
  { value: "stopped", label: "Stopped", icon: CircleMinus },
  { value: "finished", label: "Finished", icon: Circle },
];

export const SOURCES: Option<SessionSource>[] = [
  { value: "person", label: "A person", icon: User },
  { value: "task", label: "A task", icon: Play },
  { value: "session", label: "Another session", icon: CornerDownRight },
];

export function Axis<V extends string>({
  label,
  icon: AxisIcon,
  options,
  counts,
  value,
  onChange,
  wordless = false,
}: {
  label: string;
  icon: ComponentType<{ className?: string }>;
  wordless?: boolean;
  options: Option<V>[];
  counts: Record<string, number>;
  value: V[];
  onChange: (next: V[]) => void;
}) {
  const picked = options.filter((o) => value.includes(o.value));
  const toggle = (v: V) =>
    onChange(
      toggleShown(
        options.map((o) => o.value),
        value,
        v,
      ),
    );
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
        <AxisIcon className="size-4" />
        {(!wordless || picked.length > 0) && (
          <span className="hidden text-xs sm:inline">
            {picked.length === 1
              ? picked[0].label
              : picked.length > 1
                ? `${picked.length} ${label.toLowerCase()}`
                : label}
          </span>
        )}
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-52">
        <DropdownMenuGroup>
          {options.map(({ value: v, label: text, icon: Icon, tint }) => (
            <DropdownMenuCheckboxItem
              key={v}
              checked={isShown(value, v)}
              onCheckedChange={() => toggle(v)}
              closeOnClick={false}
            >
              <Icon className={cn("size-4 shrink-0", tint)} />
              <span className="min-w-0 flex-1 truncate">{text}</span>
              {(counts[v] ?? 0) > 0 && (
                <span className="text-xs tabular-nums text-muted-foreground">
                  {counts[v]}
                </span>
              )}
            </DropdownMenuCheckboxItem>
          ))}
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function AgentAxis({
  agents,
  value,
  onChange,
}: {
  agents: { id: string; name: string; count: number }[];
  value: string[];
  onChange: (next: string[]) => void;
}) {
  const byId = new Map(agents.map((b) => [b.id, b]));
  const picked = agents.filter((b) => value.includes(b.id));
  const popover = useExclusivePopover();
  const ANY = "*";
  return (
    <Combobox
      open={popover.open}
      onOpenChange={popover.onOpenChange}
      onOpenChangeComplete={popover.onOpenChangeComplete}
      multiple
      items={[ANY, ...agents.map((b) => b.id)]}
      itemToStringLabel={(id: string) =>
        id === ANY ? "Any agent" : (byId.get(id)?.name ?? id)
      }
      value={value.length ? value : [ANY]}
      onValueChange={(next: string[]) =>
        onChange(
          next.includes(ANY) && !value.includes(ANY) && next.length > 1 && value.length
            ? []
            : next.filter((id) => id !== ANY),
        )
      }
    >
      <ComboboxTrigger
        render={
          <Button
            variant="quiet"
            size="sm"
            aria-label="Agent"
            className={cn(
              "gap-1.5 px-2 [&>svg:last-of-type]:hidden",
              picked.length > 0 && "text-foreground",
            )}
          />
        }
      >
        {picked.length > 0 ? (
          <span className="flex -space-x-1.5">
            {picked.slice(0, 3).map((b) => (
              <AgentInitials
                key={b.id}
                name={b.name}
                className="size-5 text-[9px] ring-1 ring-background"
              />
            ))}
          </span>
        ) : (
          <Play className="size-4" />
        )}
        {picked.length > 0 && (
          <span className="hidden text-xs sm:inline">
            {picked.length === 1 ? picked[0].name : `${picked.length} agents`}
          </span>
        )}
      </ComboboxTrigger>
      <ComboboxContent
        ref={popover.popupRef}
        className="w-60"
        aria-label="Filter by agent"
      >
        <ComboboxInput placeholder="Find an agent" showTrigger={false} />
        <ComboboxEmpty>No agent by that name.</ComboboxEmpty>
        <ComboboxList>
          {(id: string) => {
            if (id === ANY)
              return (
                <ComboboxItem key={id} value={id}>
                  <Play className="size-4 shrink-0" />
                  <span className="min-w-0 flex-1 truncate">Any agent</span>
                  <span className="text-xs tabular-nums text-muted-foreground">
                    {agents.reduce((n, b) => n + b.count, 0)}
                  </span>
                </ComboboxItem>
              );
            const b = byId.get(id);
            if (!b) return null;
            return (
              <ComboboxItem key={id} value={id}>
                <AgentInitials name={b.name} className="size-5 text-[9px]" />
                <span className="min-w-0 flex-1 truncate">{b.name}</span>
                {b.count > 0 && (
                  <span className="text-xs tabular-nums text-muted-foreground">
                    {b.count}
                  </span>
                )}
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
  nodeDids,
  value,
  onChange,
}: {
  sessions: SessionSummary[];
  nodeDids: readonly string[];
  value: SessionFilter;
  onChange: (next: SessionFilter) => void;
}): ReactNode {
  const countFor = (axis: keyof SessionFilter, test: (c: SessionSummary) => boolean) =>
    sessions.filter((c) => passes(c, { ...value, [axis]: [] }) && test(c)).length;

  const stateCounts = Object.fromEntries(
    STATES.map((s) => [s.value, countFor("states", (c) => matchesState(c, s.value))]),
  );
  const sourceCounts = Object.fromEntries(
    SOURCES.map((s) => [
      s.value,
      countFor("sources", (c) => matchesSource(c, s.value)),
    ]),
  );
  const fleetNodes = useFleet((state) => state.nodes);
  const names = new Map<string, string>();
  for (const did of nodeDids)
    for (const b of fleetNodes[nodeKeyOf({ nodeDid: did })]?.agents ?? [])
      if (!names.has(b.agentId)) names.set(b.agentId, b.displayName);
  for (const c of sessions)
    if (c.agentId && !names.has(c.agentId))
      names.set(c.agentId, agentName(c.agentId, fleetNodes[nodeKeyOf(c)] ?? null));
  for (const id of value.agents) if (!names.has(id)) names.set(id, id);
  const agents = [...names].map(([id, name]) => ({
    id,
    name,
    count: countFor("agents", (c) => c.agentId === id),
  }));

  if (sessions.length === 0 && !hasFilter(value)) return null;

  return (
    <div
      aria-label="Session filters"
      className="flex min-w-0 shrink items-center gap-0.5 text-muted-foreground"
    >
      {agents.length > 0 && (
        <AgentAxis
          agents={agents}
          value={value.agents}
          onChange={(next) => onChange({ ...value, agents: next })}
        />
      )}
      <Axis
        label="State"
        icon={Activity}
        wordless
        options={STATES}
        counts={stateCounts}
        value={value.states}
        onChange={(states) => onChange({ ...value, states })}
      />
      <Axis
        label="Started by"
        icon={User}
        wordless
        options={SOURCES}
        counts={sourceCounts}
        value={value.sources}
        onChange={(sources) => onChange({ ...value, sources })}
      />
    </div>
  );
}
