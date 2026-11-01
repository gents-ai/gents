import { ChevronsUpDown } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { cn } from "@gents/ui/lib/utils";
import { useApp } from "@/app/AppContext";
import { useFleet } from "@/hooks/useFleet";
import { useHomeDid } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { navigate, type Route } from "@/lib/router";
import { fleetNodes } from "@/lib/scope";
import { NodeAvatar } from "@/screens/AgentAvatar";

/** Where switching to `nodeDid` leads from `route`: the same page on the
    new node where the page belongs to a node, a new session where it
    shows the old node's session, and nowhere new otherwise. */
export function switchedRoute(route: Route, nodeDid: string): Route | null {
  if (route.name === "agent") return { name: "agent", nodeDid, section: route.section };
  if (route.name === "sessions") return { ...route, nodeDid };
  if (route.name === "session" && route.sessionId !== null)
    return { name: "session", sessionId: null };
  return null;
}

/** The nav's way to change the node the app works with. */
export function NodeSwitcher({
  route,
  current,
  className,
}: {
  route: Route;
  current: string;
  className?: string;
}) {
  const { actions } = useApp();
  const homeDid = useHomeDid();
  const nodes = useFleet(useShallow(fleetNodes));
  const choose = (nodeDid: string) => {
    if (nodeDid === current) return;
    actions.selectNode(nodeDid);
    const next = switchedRoute(route, nodeDid);
    if (next) navigate(next);
  };
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="quiet"
            size="icon-xs"
            aria-label="Switch node"
            className={cn("shrink-0 text-muted-foreground", className)}
          />
        }
      >
        <ChevronsUpDown />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-64">
        <DropdownMenuRadioGroup
          value={current}
          onValueChange={(v) => choose(String(v))}
        >
          <DropdownMenuLabel>Nodes</DropdownMenuLabel>
          {nodes.map((node) => {
            const name = node.node.displayName ?? node.label;
            return (
              <DropdownMenuRadioItem key={node.nodeDid} value={node.nodeDid}>
                <span className="flex min-w-0 flex-1 items-center gap-2">
                  <span className="relative shrink-0">
                    <NodeAvatar name={name} className="size-5" />
                    <span
                      aria-hidden
                      className={cn(
                        "absolute -right-0.5 -bottom-0.5 size-2 rounded-full border-2 border-popover",
                        node.dialSucceeded ? "bg-brand" : "bg-border",
                      )}
                    />
                  </span>
                  <span className="min-w-0 truncate">{name}</span>
                  <span className="shrink-0 text-xs text-muted-foreground">
                    {isLocalNode(node, homeDid) ? "Local" : "Remote"}
                    {node.dialSucceeded ? "" : " · offline"}
                  </span>
                </span>
              </DropdownMenuRadioItem>
            );
          })}
        </DropdownMenuRadioGroup>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => navigate({ name: "nodes" })}>
          All nodes
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
