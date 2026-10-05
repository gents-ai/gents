/* Which nodes a list shows, in the sessions bar's style. "All nodes" is
   the open state: it clears the pick, and the chips narrow to some. The
   trigger wears the picked nodes' avatars, stacked when there are several,
   and the first few nodes, local first, when it is open to all. Hidden when the client
   can see only one node, since there is nothing to choose. */
import { ChevronDown, Waypoints } from "lucide-react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { cn } from "@gents/ui/lib/utils";
import { nodeDidOf, workingNode } from "@/lib/nodes";
import { AgentAvatar } from "./AgentAvatar";

export function NodeAxis({
  nodes,
  counts,
  value,
  onChange,
}: {
  nodes: readonly DeploymentView[];
  counts: Record<string, number>;
  value: string[];
  onChange: (next: string[]) => void;
}) {
  if (nodes.length < 2) return null;
  const working = workingNode(nodes);
  const nameOf = (n: DeploymentView) => n.agentPrincipal.displayName ?? n.label;
  const picked = nodes.filter((n) => value.includes(nodeDidOf(n)));
  /* open to all: the first few nodes stand in, the local one leading */
  const shown =
    picked.length > 0
      ? picked
      : [...nodes].sort((a, b) => (a === working ? -1 : b === working ? 1 : 0));
  const toggle = (v: string) =>
    onChange(value.includes(v) ? value.filter((x) => x !== v) : [...value, v]);
  const total = Object.values(counts).reduce((a, b) => a + b, 0);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="quiet"
            size="sm"
            aria-label="Node"
            className={cn("gap-1.5 px-2", picked.length > 0 && "text-foreground")}
          />
        }
      >
        {/* a phone has room for one mark on the axis: the rest come back at sm */}
        <span className="flex shrink-0 -space-x-1.5">
          {shown.slice(0, 3).map((n, i) => (
            <AgentAvatar
              key={nodeDidOf(n)}
              name={nameOf(n)}
              className={cn(
                "size-5 shrink-0 text-[9px] ring-1 ring-background",
                i > 0 && "max-sm:hidden",
              )}
            />
          ))}
        </span>
        <span className="hidden text-xs sm:inline">
          {picked.length === 1
            ? nameOf(picked[0])
            : picked.length > 1
              ? `${picked.length} nodes`
              : "All nodes"}
        </span>
        <ChevronDown className="hidden size-3 opacity-50 sm:block" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-52">
        <DropdownMenuGroup>
          <DropdownMenuCheckboxItem
            checked={picked.length === 0}
            onCheckedChange={() => onChange([])}
            closeOnClick={false}
          >
            <Waypoints className="size-4 shrink-0" />
            <span className="min-w-0 flex-1 truncate">All nodes</span>
            <span className="text-xs tabular-nums text-muted-foreground">{total}</span>
          </DropdownMenuCheckboxItem>
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuGroup>
          {nodes.map((n) => {
            const v = nodeDidOf(n);
            return (
              <DropdownMenuCheckboxItem
                key={v}
                checked={value.includes(v)}
                disabled={(counts[v] ?? 0) === 0 && !value.includes(v)}
                onCheckedChange={() => toggle(v)}
                closeOnClick={false}
              >
                <AgentAvatar name={nameOf(n)} className="size-5 text-[9px]" />
                <span className="min-w-0 flex-1 truncate">{nameOf(n)}</span>
                <span className="text-xs tabular-nums text-muted-foreground">
                  {counts[v] ?? 0}
                </span>
              </DropdownMenuCheckboxItem>
            );
          })}
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
