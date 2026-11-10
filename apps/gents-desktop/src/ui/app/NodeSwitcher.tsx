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
import { useFleet } from "@/hooks/useFleet";
import { useHomeDid } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { currentRoute, navigate, reviewNavigation, type Route } from "@/lib/router";
import { useAriaKeys, useKeysHint } from "@/commands/commands";
import { Hint } from "@/screens/Hint";
import { fleetNodes } from "@/lib/scope";
import { shownNodes, useHidden } from "@/lib/hidden";
import { NodeAvatar, nodeAvatarOf } from "@/screens/AgentAvatar";
import { NodeActivity } from "./NodeActivity";

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

/** Makes `nodeDid` the node the app works with, from `route`, asked first
    like a link to the node's page, since a draft stays with the node it was
    written for. The switcher, search and the node keys all switch this way. */
export function switchToNode(
  nodeDid: string,
  selectNode: (nodeDid: string) => void,
  route: Route | null = currentRoute(),
) {
  const next = route ? switchedRoute(route, nodeDid) : null;
  reviewNavigation({ name: "sessions", nodeDid }, () => {
    selectNode(nodeDid);
    if (next) navigate(next);
  });
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
  const keys = useKeysHint("node.switch");
  const aria = useAriaKeys("node.switch");
  const { actions } = useApp();
  const homeDid = useHomeDid();
  const hidden = useHidden();
  const nodes = shownNodes(useFleet(useShallow(fleetNodes)), hidden, current);
  /* as the Nodes page has them: this machine's first, then the remote ones */
  const groups = [
    { label: "Local", nodes: nodes.filter((n) => isLocalNode(n, homeDid)) },
    { label: "Remote", nodes: nodes.filter((n) => !isLocalNode(n, homeDid)) },
  ].filter((group) => group.nodes.length > 0);
  const choose = (nodeDid: string) => {
    if (nodeDid !== current) switchToNode(nodeDid, actions.selectNode, route);
  };
  const opener = (
    <DropdownMenuTrigger
      render={
        trigger ?? (
          <Button
            variant="quiet"
            size="icon-xs"
            aria-label="Switch node"
            aria-keyshortcuts={aria}
            className={cn("shrink-0 text-muted-foreground", className)}
          />
        )
      }
    >
      {children ?? <ChevronsUpDown />}
    </DropdownMenuTrigger>
  );
  return (
    <DropdownMenu>
      {trigger ? (
        opener
      ) : (
        <Hint label="Switch node" keys={keys}>
          {opener}
        </Hint>
      )}
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
                        <NodeAvatar
                          name={name}
                          {...nodeAvatarOf(node)}
                          className={cn("size-6", !node.dialSucceeded && "opacity-50")}
                        />
                        <span
                          aria-hidden
                          className={cn(
                            "absolute -right-0.5 -bottom-0.5 size-2 rounded-full border-2 border-popover",
                            node.dialSucceeded ? "bg-brand" : "bg-border",
                          )}
                        />
                      </span>
                      <span className="min-w-0 truncate">{name}</span>
                      {node.dialSucceeded ? (
                        <NodeActivity nodeDid={node.nodeDid} className="ml-auto" />
                      ) : (
                        // the item lights every child while highlighted; this stays quiet
                        <span className="ml-auto shrink-0 text-xs text-muted-foreground!">
                          Offline
                        </span>
                      )}
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
