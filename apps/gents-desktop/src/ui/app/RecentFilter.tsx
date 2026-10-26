import { ListFilter } from "lucide-react";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { cn } from "@gents/ui/lib/utils";
import { useFleet } from "@/hooks/useFleet";
import {
  emptyFilter,
  hasFilter,
  matchesSource,
  matchesState,
  type SessionFilter,
} from "@/lib/session-filter";
import { agentName } from "@/screens/behavior";
import { SOURCES, STATES, type Option } from "@/screens/SessionFilters";
import { nodeKeyOf } from "../../hooks/fleetStore";
import { listViews, useListViews } from "./listViews";

/** The side nav's filter on its recent sessions: the sessions list's
    agent, state and started-by choices, kept apart from that list's. */
export function RecentFilter({ sessions }: { sessions: SessionSummary[] }) {
  const value = useListViews((views) => views.recentFilter);
  const nodes = useFleet((state) => state.nodes);
  const active = hasFilter(value);
  const agents = [...new Set(sessions.flatMap((c) => (c.agentId ? [c.agentId] : [])))]
    .map((id) => {
      const holder = sessions.find((c) => c.agentId === id)!;
      return { id, name: agentName(id, nodes[nodeKeyOf(holder)] ?? null) };
    })
    .sort((a, b) => a.name.localeCompare(b.name));
  const toggle = <K extends keyof SessionFilter>(
    axis: K,
    v: SessionFilter[K][number],
  ) => {
    const picked = value[axis] as string[];
    listViews.setRecentFilter({
      ...value,
      [axis]: picked.includes(v) ? picked.filter((p) => p !== v) : [...picked, v],
    });
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="quiet"
            size="icon-xs"
            aria-label="Filter recent sessions"
            className={cn("shrink-0", active && "text-foreground")}
          />
        }
      >
        <ListFilter />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-52">
        <DropdownMenuGroup>
          <DropdownMenuLabel>Filters</DropdownMenuLabel>
          {agents.length > 1 && (
            <Axis
              label="Agent"
              picked={value.agents.length}
              options={agents.map(({ id, name }) => ({
                value: id,
                label: name,
                count: sessions.filter((c) => c.agentId === id).length,
                checked: value.agents.includes(id),
                onToggle: () => toggle("agents", id),
              }))}
            />
          )}
          <Axis
            label="State"
            picked={value.states.length}
            options={STATES.map((o) => ({
              ...o,
              count: sessions.filter((c) => matchesState(c, o.value)).length,
              checked: value.states.includes(o.value),
              onToggle: () => toggle("states", o.value),
            }))}
          />
          <Axis
            label="Started by"
            picked={value.sources.length}
            options={SOURCES.map((o) => ({
              ...o,
              count: sessions.filter((c) => matchesSource(c, o.value)).length,
              checked: value.sources.includes(o.value),
              onToggle: () => toggle("sources", o.value),
            }))}
          />
        </DropdownMenuGroup>
        {active && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => listViews.setRecentFilter(emptyFilter)}>
              Clear filters
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function Axis({
  label,
  picked,
  options,
}: {
  label: string;
  picked: number;
  options: (Pick<Option<string>, "value" | "label" | "tint"> & {
    icon?: Option<string>["icon"];
    count: number;
    checked: boolean;
    onToggle: () => void;
  })[];
}) {
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        <span className="flex-1">{label}</span>
        {picked > 0 && (
          <span className="text-xs tabular-nums text-muted-foreground">{picked}</span>
        )}
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="w-52">
        {options.map(
          ({ value, label: text, icon: Icon, tint, count, checked, onToggle }) => (
            <DropdownMenuCheckboxItem
              key={value}
              checked={checked}
              onCheckedChange={onToggle}
              closeOnClick={false}
            >
              {Icon && <Icon className={cn("size-4 shrink-0", tint)} />}
              <span className="min-w-0 flex-1 truncate">{text}</span>
              <span className="text-xs tabular-nums text-muted-foreground">
                {count}
              </span>
            </DropdownMenuCheckboxItem>
          ),
        )}
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}
