import {
  cloneElement,
  Fragment,
  useState,
  type ReactElement,
  type ReactNode,
} from "react";
import { Check, ChevronsUpDown, Waypoints } from "lucide-react";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetTitle,
} from "@gents/ui/components/sheet";
import { useMediaQuery } from "@/lib/media";
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
import {
  projectRuntimeConnectionStatus,
  runtimeConnectionBlocksAdmission,
} from "@source-inc/gents-desktop-client";
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
  /* a phone gets a sheet of full-width rows, not a desktop menu off a caret */
  const narrow = useMediaQuery("(max-width: 767px)");
  const [sheetOpen, setSheetOpen] = useState(false);
  if (narrow) {
    return (
      <>
        {trigger ? (
          cloneElement(
            trigger as ReactElement<{ onClick?: () => void }>,
            { onClick: () => setSheetOpen(true) },
            children,
          )
        ) : (
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Switch node"
            onClick={() => setSheetOpen(true)}
            className={cn(
              "me-1 shrink-0 bg-secondary text-muted-foreground",
              className,
            )}
          >
            {children ?? <ChevronsUpDown />}
          </Button>
        )}
        <Sheet open={sheetOpen} onOpenChange={setSheetOpen}>
          <SheetContent
            side="bottom"
            showCloseButton={false}
            className="max-h-[80vh] gap-1 rounded-t-2xl border border-b-0 border-border/50 px-2 pt-2 pb-[max(1rem,env(safe-area-inset-bottom))]"
          >
            <span
              aria-hidden
              className="mx-auto mb-2 h-1 w-10 rounded-full bg-border"
            />
            <div className="px-3 pb-2">
              <SheetTitle>Switch node</SheetTitle>
              <SheetDescription className="text-xs">
                The node this app works with.
              </SheetDescription>
            </div>
            <div className="min-h-0 overflow-y-auto">
              {groups.map((group) => (
                <div key={group.label} className="pb-2">
                  <p className="px-3 pt-2 pb-1 text-xs text-muted-foreground">
                    {group.label}
                  </p>
                  {group.nodes.map((node) => {
                    const name = node.node.displayName ?? node.label;
                    const online = !runtimeConnectionBlocksAdmission(
                      node.runtimeConnection,
                    );
                    const selected = node.nodeDid === current;
                    return (
                      <button
                        key={node.nodeDid}
                        type="button"
                        aria-pressed={selected}
                        onClick={() => {
                          setSheetOpen(false);
                          choose(node.nodeDid);
                        }}
                        className={cn(
                          "flex min-h-14 w-full items-center gap-3 rounded-xl px-3 text-start active:bg-accent",
                          selected && "bg-accent/60",
                        )}
                      >
                        <span className="relative shrink-0">
                          <NodeAvatar
                            name={name}
                            {...nodeAvatarOf(node)}
                            className={cn("size-9", !online && "opacity-50")}
                          />
                          <span
                            aria-hidden
                            className={cn(
                              "absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-popover",
                              online ? "bg-brand" : "bg-border",
                            )}
                          />
                        </span>
                        <span className="min-w-0 flex-1">
                          <span className="block truncate text-sm font-medium text-heading">
                            {name}
                          </span>
                          <span className="block truncate text-xs text-muted-foreground">
                            {online
                              ? group.label === "Local"
                                ? "On this device"
                                : "Connected"
                              : projectRuntimeConnectionStatus(node.runtimeConnection)
                                  .shortLabel}
                          </span>
                        </span>
                        {selected && (
                          <Check
                            className="size-4 shrink-0 text-brand"
                            aria-label="Current"
                          />
                        )}
                      </button>
                    );
                  })}
                </div>
              ))}
            </div>
            <button
              type="button"
              onClick={() => {
                setSheetOpen(false);
                navigate({ name: "nodes" });
              }}
              className="flex min-h-12 w-full items-center gap-3 rounded-xl border-t border-border/60 px-3 text-sm text-muted-foreground active:bg-accent"
            >
              <Waypoints className="size-4" /> All nodes
            </button>
          </SheetContent>
        </Sheet>
      </>
    );
  }
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
                const online = !runtimeConnectionBlocksAdmission(
                  node.runtimeConnection,
                );
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
                          className={cn("size-6", !online && "opacity-50")}
                        />
                        <span
                          aria-hidden
                          className={cn(
                            "absolute -right-0.5 -bottom-0.5 size-2 rounded-full border-2 border-popover",
                            online ? "bg-brand" : "bg-border",
                          )}
                        />
                      </span>
                      <span className="min-w-0 truncate">{name}</span>
                      {online ? (
                        <NodeActivity nodeDid={node.nodeDid} className="ml-auto" />
                      ) : (
                        // the item lights every child while highlighted; this stays quiet
                        <span className="ml-auto shrink-0 text-xs text-muted-foreground!">
                          {
                            projectRuntimeConnectionStatus(node.runtimeConnection)
                              .shortLabel
                          }
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
