import { useContext, type ReactNode } from "react";
import { Inbox, Plus, ScrollText, Waypoints } from "lucide-react";
import { Tooltip, TooltipContent, TooltipTrigger } from "@gents/ui/components/tooltip";
import { cn } from "@gents/ui/lib/utils";
import { href, type Route } from "@/lib/router";
import { AgentAvatar } from "@/screens/AgentAvatar";
import { AgentHoverCard } from "@/screens/HoverCards";
import { FlyoutOpenContext } from "./RailFlyout";
import { SyncHealth } from "./SyncHealth";
import {
  useMailboxCount,
  useOnline,
  useSelectedNode,
  useSyncHealth,
  useToolAuthority,
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

/* The selected node: its avatar opens its configuration, with a card on
   hover. With no node selected the slot says so and goes to the Nodes list,
   as the panel's does. */
function RailNode({ route }: { route: Route }) {
  const node = useSelectedNode();
  const online = useOnline();
  const { root, ceiling } = useToolAuthority();
  const agentName = node?.agentPrincipal.displayName ?? null;
  if (!node)
    return (
      <a
        href={href({ name: "nodes" })}
        aria-label="No local node"
        title="No local node"
        className="mb-2 grid size-7 place-items-center rounded-full border border-dashed border-border text-muted-foreground hover:text-foreground"
      >
        <Waypoints className="size-3.5" />
      </a>
    );
  return (
    <AgentHoverCard deployment={node} root={root} ceiling={ceiling}>
      <a
        href={href({ name: "agent", agentDid: node.agentDid, section: "agent" })}
        aria-label={`${agentName ?? "Agent"} configuration`}
        aria-current={route.name === "agent" ? "page" : undefined}
        className={cn(
          "relative mb-2 block size-7 rounded-full ring-1 ring-border ring-offset-2 ring-offset-background transition-shadow hover:ring-muted-foreground",
          route.name === "agent" && "ring-2 ring-ink",
        )}
      >
        <AgentAvatar name={agentName ?? "Agent"} className="size-7" />
        <span
          className={cn(
            "absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-background",
            online ? "bg-brand" : "bg-border",
          )}
          aria-hidden="true"
        />
      </a>
    </AgentHoverCard>
  );
}

/* The rail's sync chip is one control in both of the rail's states: the
   dot while the rail stands alone, and the same button widened into the
   panel's row form, drawn over the hover flyout's foot, while the flyout
   is open. One element, so nothing swaps under a pointer that opened the
   flyout on its way to it. The standing panel and the phone sheet draw
   their own row; no rail dot exists beside those. */
function RailSyncDot() {
  const flyoutOpen = useContext(FlyoutOpenContext);
  const syncHealth = useSyncHealth();
  return (
    <div className="relative h-8 w-7">
      <div
        className={cn(
          "absolute top-0 left-0 z-40",
          /* the flyout sits 8px in with 12px row margins: 20px from the
             rail's edge, 6px right of the dot's own slot */
          flyoutOpen ? "left-[6px] w-[16.5rem]" : "w-7",
        )}
      >
        <SyncHealth syncHealth={syncHealth} compact={!flyoutOpen} row={flyoutOpen} />
      </div>
    </div>
  );
}

/** The collapsed side nav: the selected node, new session, mailbox and
    sessions, with sync, nodes and settings at the foot. */
export function Rail({ route, settings }: { route: Route; settings: ReactNode }) {
  const mailboxCount = useMailboxCount();
  return (
    <nav className="flex h-full flex-col items-center gap-2 pt-3">
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
          (route.name === "session" && route.sessionId !== null)
        }
      >
        <ScrollText className="size-4" />
      </RailItem>
      {/* 17px: the flyout's 8px inset, its 8px padding and its 1px border, so
          the foot's items sit exactly where the panel draws them */}
      <div className="mt-auto flex flex-col items-center gap-1 pb-[17px]">
        <RailSyncDot />
        <div className="h-px w-8 bg-border" />
        <RailItem
          label="Agents"
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
