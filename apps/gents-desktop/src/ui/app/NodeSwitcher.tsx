import { Fragment, type ReactElement, type ReactNode } from "react";
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
import { activityOf, activityText, useFleet } from "@/hooks/useFleet";
import { useHomeDid } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { navigate, reviewNavigation, type Route } from "@/lib/router";
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

/** The nav's way to change the node the app works with, and the node
    name's in New session's heading. */
export function NodeSwitcher({
  route,
  current,
  className,
  trigger,
  children,
}: {
  route: Route;
  current: string;
  className?: string;
  /** the element that opens it, in place of the nav's ⇅ button */
  trigger?: ReactElement;
  children?: ReactNode;
}) {
  const { actions } = useApp();
  const homeDid = useHomeDid();
  const nodes = useFleet(useShallow(fleetNodes));
  /* what each node has going on, as the Nodes page counts it */
  const doing = useFleet(
    useShallow((state) =>
      Object.fromEntries(
        fleetNodes(state).map((node) => [
          node.nodeDid,
          activityText(activityOf(state, node.nodeDid)),
        ]),
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
    const next = switchedRoute(route, nodeDid);
    /* asked first like a link to the node's page, since a draft stays
       with the node it was written for */
    reviewNavigation({ name: "sessions", nodeDid }, () => {
      actions.selectNode(nodeDid);
      if (next) navigate(next);
    });
  };
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          trigger ?? (
            <Button
              variant="quiet"
              size="icon-xs"
              aria-label="Switch node"
              className={cn("shrink-0 text-muted-foreground", className)}
            />
          )
        }
      >
        {children ?? <ChevronsUpDown />}
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
