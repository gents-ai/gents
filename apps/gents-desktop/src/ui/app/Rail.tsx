import { useContext, type ReactNode } from "react";
import { Inbox, Plus, ScrollText, Waypoints } from "lucide-react";
import { Tooltip, TooltipContent, TooltipTrigger } from "@gents/ui/components/tooltip";
import { cn } from "@gents/ui/lib/utils";
import { href, type Route } from "@/lib/router";
import { NodeAvatar } from "@/screens/AgentAvatar";
import { NodeHoverCard } from "@/screens/HoverCards";
import { FlyoutOpenContext } from "./RailFlyout";
import { SyncHealth } from "./SyncHealth";
import { navRowState, useNavItems, type NavContext, type NavItem } from "./navRegistry";
import { ContribBoundary } from "@/contrib/react/boundary";
import {
  useMailboxCount,
  useOnline,
  useSelectedNode,
  useSyncHealth,
} from "@/hooks/useClient";

function RailItem({
  label,
  active,
  count,
  to,
  children,
}: {
  label: string;
  active?: boolean;
  count?: number;
  to: Route;
  children: ReactNode;
}) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <a
            href={href(to)}
            aria-label={label}
            aria-current={active ? "page" : undefined}
            className={cn(
              "relative grid size-8 place-items-center rounded-lg border border-transparent text-muted-foreground transition-colors hover:text-foreground",
              active && "border-border/60 bg-raised text-ink shadow-xs",
            )}
          />
        }
      >
        {children}
        {count ? (
          <span className="absolute -top-1.5 -right-1.5 grid h-4 min-w-4 place-items-center rounded-full bg-brand px-1 font-mono text-[10px] leading-none text-brand-foreground">
            {count}
          </span>
        ) : null}
      </TooltipTrigger>
      <TooltipContent side="right">{label}</TooltipContent>
    </Tooltip>
  );
}

function RailNode({ route }: { route: Route }) {
  const node = useSelectedNode();
  const online = useOnline();
  const nodeName = node?.node.displayName ?? null;
  if (!node)
    return (
      <a
        href={href({ name: "nodes" })}
        aria-label="No active node"
        title="No active node"
        className="mb-2 grid size-7 place-items-center rounded-full border border-dashed border-border text-muted-foreground hover:text-foreground"
      >
        <Waypoints className="size-3.5" />
      </a>
    );
  return (
    <NodeHoverCard deployment={node}>
      <a
        href={href({ name: "agent", nodeDid: node.nodeDid, section: "agent" })}
        aria-label={`${nodeName ?? "Node"} configuration`}
        aria-current={route.name === "agent" ? "page" : undefined}
        className={cn(
          "relative mb-2 block size-7 rounded-full ring-1 ring-border ring-offset-2 ring-offset-background transition-shadow hover:ring-muted-foreground",
          route.name === "agent" && "ring-2 ring-ink",
        )}
      >
        <NodeAvatar name={nodeName ?? "Node"} className="size-7" />
        <span
          className={cn(
            "absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-background",
            online ? "bg-brand" : "bg-border",
          )}
          aria-hidden="true"
        />
      </a>
    </NodeHoverCard>
  );
}

function RailSyncDot() {
  const flyoutOpen = useContext(FlyoutOpenContext);
  const syncHealth = useSyncHealth();
  return (
    <div className="relative h-8 w-7">
      <div
        className={cn(
          "absolute z-40",
          /* the dot keeps its place as it becomes the row, which spans the
             flyout's rows */
          flyoutOpen ? "-top-0.5 -left-px w-[261px]" : "top-0 left-0 w-7",
        )}
      >
        <SyncHealth syncHealth={syncHealth} compact={!flyoutOpen} row={flyoutOpen} />
      </div>
    </div>
  );
}

export function Rail({ route, settings }: { route: Route; settings: ReactNode }) {
  const mailboxCount = useMailboxCount();
  const selected = useSelectedNode();
  const primary = useNavItems("primary");
  const footer = useNavItems("footer");
  const navContext: NavContext = {
    mailboxCount,
    selectedNodeDid: selected?.nodeDid ?? null,
  };
  /* a row a UX plugin contributed: its predicates are evaluated apart and
     its icon drawn behind a boundary, so one bad row costs that row; a row
     with nowhere to go right now is not drawn */
  const contributed = (item: NavItem) => {
    const { to, active, count } = navRowState(item, route, navContext);
    if (!to) return null;
    return (
      <ContribBoundary key={item.id} id={item.id} variant="chip">
        <RailItem label={item.label} to={to} active={active} count={count}>
          {item.icon}
        </RailItem>
      </ContribBoundary>
    );
  };
  return (
    /* the flyout and the pinned panel start their node at this height */
    <nav className="flex h-full flex-col items-center gap-2 pt-[23px]">
      <RailNode route={route} />
      <div className="mb-1 h-px w-5 bg-border" />
      <RailItem
        label="New session"
        to={{ name: "session", sessionId: null }}
        active={route.name === "session" && route.sessionId === null}
      >
        <Plus className="size-4" />
      </RailItem>
      <RailItem
        label="Mailbox"
        to={{ name: "mailbox" }}
        active={route.name === "mailbox"}
        count={mailboxCount}
      >
        <Inbox className="size-4" />
      </RailItem>
      <RailItem
        label="Sessions"
        to={{ name: "sessions" }}
        active={
          route.name === "sessions" ||
          route.name === "sessionsUnder" ||
          route.name === "sessionsArchived" ||
          (route.name === "session" && route.sessionId !== null)
        }
      >
        <ScrollText className="size-4" />
      </RailItem>
      {primary.map(contributed)}
      <div className="mt-auto flex flex-col items-center gap-1 pb-[17px]">
        <RailSyncDot />
        <div className="h-px w-8 bg-border" />
        {footer.map(contributed)}
        <RailItem
          label="Nodes"
          to={{ name: "nodes" }}
          active={route.name === "nodes" || route.name === "agents"}
        >
          <Waypoints className="size-4" />
        </RailItem>
        {settings}
      </div>
    </nav>
  );
}
