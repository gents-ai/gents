import { ChevronRight } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { cn } from "@gents/ui/lib/utils";
import { isLocalNode } from "@/lib/firstRun";
import { href, navigate, reviewNavigation, type Route } from "@/lib/router";
import { fleetNodes } from "@/lib/scope";
import { shownNodes, useHidden } from "@/lib/hidden";
import { useFleet } from "@/hooks/useFleet";
import { useHomeDid } from "@/hooks/useClient";
import { NodeAvatar } from "@/screens/AgentAvatar";
import { useApp } from "./AppContext";
import { NodeActivity } from "./NodeActivity";
import { switchedRoute } from "./NodeSwitcher";

/** The side nav's Nodes section: every node not hidden on this device,
    this machine's first, each with what it has going on, and a click
    switches to it as the node switcher does. The panel leaves it out while
    the only node is this machine's. */
export function NavNodes({ route, current }: { route: Route; current: string | null }) {
  const { actions } = useApp();
  const homeDid = useHomeDid();
  const nodes = shownNodes(useFleet(useShallow(fleetNodes)), useHidden(), current);
  const ordered = [
    ...nodes.filter((n) => isLocalNode(n, homeDid)),
    ...nodes.filter((n) => !isLocalNode(n, homeDid)),
  ];
  const choose = (nodeDid: string) => {
    if (nodeDid === current) return;
    const next = switchedRoute(route, nodeDid);
    /* asked first, as the node switcher asks, since a draft stays with the
       node it was written for */
    reviewNavigation({ name: "sessions", nodeDid }, () => {
      actions.selectNode(nodeDid);
      if (next) navigate(next);
    });
  };
  return (
    <>
      <p className="mt-2 mb-0 px-5 font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
        Nodes
      </p>
      <ul className="px-3" aria-label="Nodes">
        {ordered.map((node) => {
          const name = node.node.displayName ?? node.label;
          const here = node.nodeDid === current;
          return (
            <li key={node.nodeDid}>
              <button
                type="button"
                aria-current={here ? "true" : undefined}
                onClick={() => choose(node.nodeDid)}
                className={cn(
                  "flex h-8 w-full items-center gap-2.5 rounded-lg px-2 text-left text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
                  here && "text-foreground",
                )}
              >
                {/* in the slot a session's status takes, so every title in
                    the nav starts on one line */}
                <span className="relative grid size-4 shrink-0 place-items-center">
                  <NodeAvatar
                    name={name}
                    className={cn("size-4", !node.dialSucceeded && "opacity-50")}
                  />
                  <span
                    aria-hidden
                    className={cn(
                      "absolute -right-0.5 -bottom-0.5 size-1.5 rounded-full ring-2 ring-background",
                      node.dialSucceeded ? "bg-brand" : "bg-border",
                    )}
                  />
                </span>
                <span className="min-w-0 flex-1 truncate">{name}</span>
                {node.dialSucceeded ? (
                  <NodeActivity nodeDid={node.nodeDid} />
                ) : (
                  <span className="shrink-0 text-muted-foreground">Offline</span>
                )}
              </button>
            </li>
          );
        })}
      </ul>
      <a
        href={href({ name: "nodes" })}
        className="flex items-center gap-1 px-5 py-1 text-xs text-muted-foreground hover:text-foreground"
      >
        All nodes <ChevronRight className="size-3" />
      </a>
    </>
  );
}
