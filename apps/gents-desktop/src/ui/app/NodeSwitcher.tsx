import { Fragment } from "react";
import { ChevronsUpDown, Waypoints } from "lucide-react";
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
import { NO_SESSIONS, useFleet } from "@/hooks/useFleet";
import { isLive } from "@/lib/live";
import { nodeKeyOf } from "../../hooks/fleetStore";
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
  /* what each node has going on, as the Nodes page counts it */
  const doing = useFleet(
    useShallow((state) =>
      Object.fromEntries(
        fleetNodes(state).map((node) => {
          const key = nodeKeyOf(node);
          const running = (state.sessionsOf[key] ?? NO_SESSIONS).filter((c) =>
            isLive(c.turnState),
          ).length;
          const waiting = (state.mailboxOf[key] ?? []).filter(
            (m) => m.status === "open",
          ).length;
          return [
            node.nodeDid,
            [running && `${running} running`, waiting && `${waiting} waiting`]
              .filter(Boolean)
              .join(" · "),
          ];
        }),
      ),
    ),
  );
  /* as the Nodes page has them: this machine's first, then the remote ones */
  const groups = [
    { label: "Local", nodes: nodes.filter((n) => isLocalNode(n, homeDid)) },
    { label: "Remote", nodes: nodes.filter((n) => !isLocalNode(n, homeDid)) },
  ].filter((group) => group.nodes.length > 0);
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
          {groups.map((group, i) => (
            <Fragment key={group.label}>
              {i > 0 && <DropdownMenuSeparator />}
              <DropdownMenuLabel>{group.label}</DropdownMenuLabel>
              {group.nodes.map((node) => {
                const name = node.node.displayName ?? node.label;
                return (
                  <DropdownMenuRadioItem
                    key={node.nodeDid}
                    value={node.nodeDid}
                    closeOnClick
                  >
                    <span className="flex min-w-0 flex-1 items-center gap-2.5">
                      <span className="relative shrink-0">
                        <NodeAvatar name={name} className="size-6" />
                        <span
                          aria-hidden
                          className={cn(
                            "absolute -right-0.5 -bottom-0.5 size-2 rounded-full border-2 border-popover",
                            node.dialSucceeded ? "bg-brand" : "bg-border",
                          )}
                        />
                      </span>
                      <span className="min-w-0 truncate">{name}</span>
                      <span className="ml-auto shrink-0 text-xs text-muted-foreground tabular-nums">
                        {node.dialSucceeded ? doing[node.nodeDid] : "Offline"}
                      </span>
                    </span>
                  </DropdownMenuRadioItem>
                );
              })}
            </Fragment>
          ))}
        </DropdownMenuRadioGroup>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => navigate({ name: "nodes" })}>
          <Waypoints /> All nodes
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
