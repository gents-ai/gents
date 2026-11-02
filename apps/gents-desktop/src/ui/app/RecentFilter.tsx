import { MoreHorizontal } from "lucide-react";
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
import { useSelectedNodeDid } from "@/hooks/useClient";
import {
  isShown,
  matchesSource,
  matchesState,
  sameFilter,
  toggleShown,
  type SessionFilter,
} from "@/lib/session-filter";
import {
  SOURCES,
  STATES,
  useAgentChoices,
  type Option,
} from "@/screens/SessionFilters";
import { listViews, recentDefault, useListViews } from "./listViews";

/** The side nav's options for its recent sessions: whether sessions other
    sessions started show, then the sessions list's agent, state and
    started-by filters, kept apart from that list's. */
export function RecentFilter({ sessions }: { sessions: SessionSummary[] }) {
  const value = useListViews((views) => views.recentFilter);
  const active = !sameFilter(value, recentDefault);
  const selected = useSelectedNodeDid();
  const nodeDids = [
    ...new Set([...(selected ? [selected] : []), ...sessions.map((c) => c.nodeDid)]),
  ];
  const agents = useAgentChoices(
    sessions,
    nodeDids,
    value.agents,
    (id) => sessions.filter((c) => c.agentId === id).length,
  );
  const agentIds = agents.map((a) => a.id);
  const toggle = <K extends keyof SessionFilter>(
    axis: K,
    all: readonly string[],
    v: SessionFilter[K][number],
  ) =>
    listViews.setRecentFilter({
      ...value,
      [axis]: toggleShown(all, value[axis] as string[], v),
    });

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="quiet"
            size="icon-xs"
            aria-label="Recent sessions options"
            className={cn("shrink-0", active && "text-foreground")}
          />
        }
      >
        <MoreHorizontal />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-64">
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
                checked: isShown(value.agents, id),
                onToggle: () => toggle("agents", agentIds, id),
              }))}
            />
          )}
          <Axis
            label="State"
            picked={value.states.length}
            options={STATES.map((o) => ({
              ...o,
              count: sessions.filter((c) => matchesState(c, o.value)).length,
              checked: isShown(value.states, o.value),
              onToggle: () =>
                toggle(
                  "states",
                  STATES.map((s) => s.value),
                  o.value,
                ),
            }))}
          />
          <Axis
            label="Started by"
            picked={value.sources.length}
            options={SOURCES.map((o) => ({
              ...o,
              count: sessions.filter((c) => matchesSource(c, o.value)).length,
              checked: isShown(value.sources, o.value),
              onToggle: () =>
                toggle(
                  "sources",
                  SOURCES.map((s) => s.value),
                  o.value,
                ),
            }))}
          />
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuCheckboxItem
          checked={isShown(value.sources, "session")}
          onCheckedChange={() =>
            toggle(
              "sources",
              SOURCES.map((s) => s.value),
              "session",
            )
          }
          closeOnClick={false}
        >
          Show sub-sessions
        </DropdownMenuCheckboxItem>
        {active && (
          <DropdownMenuItem onClick={() => listViews.setRecentFilter(recentDefault)}>
            Reset filters
          </DropdownMenuItem>
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
              {count > 0 && (
                <span className="text-xs tabular-nums text-muted-foreground">
                  {count}
                </span>
              )}
            </DropdownMenuCheckboxItem>
          ),
        )}
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}
