/* The rail, expanded: hovering the side nav slides out a raised panel
   over the canvas with the same items in full (agent, new session,
   mailbox, sessions) and a quick list of recent sessions, the way a
   collapsed sidebar opens on hover. It overlays rather than pushes, so
   the screen behind never reflows, and its rows sit on the rail's own
   grid (same top padding, heights and gaps, icons in the same 32px slot
   12px from the edge) so nothing appears to move when it opens. Opens
   after a short rest, closes when the pointer leaves; keyboard focus
   inside keeps it open. */
import { createContext } from "react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronRight, Inbox, Lock, Plus, ScrollText, Waypoints } from "lucide-react";
import type { DeploymentView, SessionSummary } from "@source-inc/gents-desktop-client";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import { PortalContainerProvider } from "@gents/ui/lib/portal-container";
import { href, type Route } from "@/lib/router";
import type { NavMode } from "@/nav";
import { AgentAvatar } from "@/screens/AgentAvatar";
import { SessionStatus } from "@/screens/SessionStatus";

const OPEN_AFTER = 80;
const CLOSE_AFTER = 180;
const RECENT = 8;

function Item({
  to,
  active,
  count,
  icon,
  children,
}: {
  to: string;
  active?: boolean;
  count?: number;
  icon: ReactNode;
  children: ReactNode;
}) {
  return (
    <a
      href={to}
      aria-current={active ? "page" : undefined}
      className={cn(
        "mx-3 flex h-8 items-center gap-2 rounded-lg border border-transparent pr-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
        active && "border-border/60 bg-background text-ink shadow-xs",
      )}
    >
      {/* the rail's 32px square, minus the border already on the row */}
      <span className="grid size-[30px] shrink-0 place-items-center">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {count ? (
        <span className="grid h-4 min-w-4 place-items-center rounded-full bg-brand px-1 font-mono text-[10px] leading-none text-brand-foreground">
          {count}
        </span>
      ) : null}
    </a>
  );
}

/* the panel's content, shared by the hover flyout, the expanded column
   and the mobile sheet */
export function NavPanel({
  route,
  deployment,
  online,
  mailboxCount,
  recent: recentProp,
  working,
  nodeCount,
  holds = new Set<string>(),
  settings,
  foot,
}: {
  route: Route;
  agentName: string | null;
  agentDid: string | null;
  deployment: DeploymentView | null;
  online: boolean;
  mailboxCount: number;
  holds?: Set<string>;
  /** the settings menu, as a row at the foot */
  settings?: ReactNode;
  /** what sits above Nodes at the foot: the sync chip, from the app */
  foot?: ReactNode;
  /** the newest sessions in scope, from the app; absent, the node's own */
  recent?: SessionSummary[];
  /** the node this machine runs; null when the client is paired only to remote nodes */
  working?: DeploymentView | null;
  /** how many nodes the client can see */
  nodeCount?: number;
}) {
  const recent =
    recentProp ??
    [...(deployment?.sessions ?? [])]
      .sort((a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""))
      .slice(0, RECENT);
  const currentSession = route.name === "session" ? route.sessionId : null;
  return (
    <>
      {/* the working node, in the same slot as its avatar on the rail: its
          configuration is always one click away. A client that runs no node
          of its own says so, and offers the nodes it can see. */}
      {working ? (
        <a
          href={href({
            name: "agent",
            agentDid: working.agentDid,
            section: "agent",
          })}
          aria-current={
            route.name === "agent" && route.agentDid === working.agentDid
              ? "page"
              : undefined
          }
          data-testid="working-node"
          className={cn(
            "group mr-3 mb-2 ml-[14px] flex h-7 items-center gap-3 rounded-lg text-sm transition-colors",
            route.name === "agent" && route.agentDid === working.agentDid && "text-ink",
          )}
        >
          <span
            className={cn(
              "relative block size-7 shrink-0 rounded-full ring-1 ring-border ring-offset-2 ring-offset-raised transition-shadow group-hover:ring-muted-foreground",
              route.name === "agent" &&
                route.agentDid === working.agentDid &&
                "ring-2 ring-ink",
            )}
          >
            <AgentAvatar
              name={working.agentPrincipal.displayName ?? working.label}
              className="size-7"
            />
            <span
              className={cn(
                "absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-raised",
                online ? "bg-brand" : "bg-border",
              )}
              aria-hidden="true"
            />
          </span>
          <span className="min-w-0 flex-1">
            <span className="block truncate font-heading font-medium text-heading">
              {working.agentPrincipal.displayName ?? working.label}
            </span>
            <span className="block text-xs text-muted-foreground">
              Local node · Configure
            </span>
          </span>
        </a>
      ) : (
        <a
          href={href({ name: "nodes" })}
          data-testid="no-working-node"
          className="group mr-3 mb-2 ml-[14px] flex h-7 items-center gap-3 rounded-lg text-sm text-muted-foreground transition-colors hover:text-foreground"
        >
          <span className="grid size-7 shrink-0 place-items-center rounded-full ring-1 ring-dashed ring-border">
            <Waypoints className="size-3.5" />
          </span>
          <span className="min-w-0 flex-1">
            <span className="block truncate font-heading font-medium">
              No local node
            </span>
            <span className="block text-xs">Working on remote nodes</span>
          </span>
        </a>
      )}
      {/* the rail's hairline, in place, then drawn on to the edge */}
      <div className="mb-1 ml-[18px] h-px w-5 bg-border" />
      <Item
        to={href({ name: "session", sessionId: null })}
        active={route.name === "session" && route.sessionId === null}
        icon={<Plus className="size-4" />}
      >
        New session
      </Item>
      <Item
        to={href({ name: "mailbox" })}
        active={route.name === "mailbox"}
        count={mailboxCount}
        icon={<Inbox className="size-4" />}
      >
        Mailbox
      </Item>
      <Item
        to={href({ name: "sessions" })}
        active={route.name === "sessions"}
        icon={<ScrollText className="size-4" />}
      >
        Sessions
      </Item>
      {recent.length > 0 && (
        <>
          <p className="mt-2 mb-0 px-5 font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
            Recent sessions
          </p>
          <ScrollArea className="min-h-0">
            <ul className="px-3">
              {recent.map((c) => (
                <li key={c.sessionId}>
                  <a
                    href={href({ name: "session", sessionId: c.sessionId })}
                    aria-current={currentSession === c.sessionId ? "page" : undefined}
                    className={cn(
                      "flex h-8 items-center gap-2.5 rounded-lg px-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
                      currentSession === c.sessionId && "text-foreground",
                    )}
                  >
                    <SessionStatus
                      turnState={c.turnState}
                      held={holds.has(c.sessionId)}
                    />
                    <span className="min-w-0 flex-1 truncate">
                      {c.title ?? "Untitled session"}
                    </span>
                    {c.unreadableReason && (
                      <Lock
                        className="size-3 shrink-0"
                        role="img"
                        aria-label={c.unreadableReason}
                      >
                        <title>{c.unreadableReason}</title>
                      </Lock>
                    )}
                  </a>
                </li>
              ))}
            </ul>
          </ScrollArea>
          <a
            href={href({ name: "sessions" })}
            className="flex items-center gap-1 px-5 py-1 text-xs text-muted-foreground hover:text-foreground"
          >
            All sessions <ChevronRight className="size-3" />
          </a>
        </>
      )}
      {/* the same rhythm as the rail's foot (AppShell): 4px between chip, rule, Nodes and Settings */}
      <div className="mt-auto flex flex-col gap-1">
        {foot}
        <div className="mx-3 h-px bg-border" />
        <Item
          to={href({ name: "nodes" })}
          active={route.name === "nodes" || route.name === "agents"}
          count={nodeCount}
          icon={<Waypoints className="size-4" />}
        >
          Nodes
        </Item>
        {settings}
      </div>
    </>
  );
}

/* whether the hover flyout is showing: what the rail draws under the
   flyout's own rows hides itself while it is, so there is one control */
export const FlyoutOpenContext = createContext(false);

export function RailFlyout({
  mark,
  route,
  agentName,
  agentDid,
  deployment,
  online,
  mailboxCount,
  holds = new Set<string>(),
  mode = "hover",
  settings,
  foot,
  recent,
  working,
  nodeCount,
  children,
}: {
  route: Route;
  agentName: string | null;
  agentDid: string | null;
  deployment: DeploymentView | null;
  online: boolean;
  mailboxCount: number;
  /** sessions with a held tool call waiting on a person */
  holds?: Set<string>;
  /** hover: rail with a flyout; expanded: the panel in the flow; collapsed: rail only */
  mode?: NavMode;
  settings?: ReactNode;
  /** the newest sessions in scope, from the app */
  recent?: SessionSummary[];
  working?: DeploymentView | null;
  nodeCount?: number;
  /** the collapsed rail */
  foot?: ReactNode;
  /** the rail's mark: drawn above the expanded panel, left visible beside the flyout */
  mark?: ReactNode;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  /* menus opened from the flyout render inside it rather than at the end
     of the body, so moving the pointer or focus into one is not leaving
     the flyout, and it stays open while the menu is used */
  const [portal, setPortal] = useState<HTMLElement | null>(null);
  const timer = useRef<number | null>(null);
  const later = (next: boolean, ms: number) => {
    if (timer.current) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setOpen(next), ms);
  };
  useEffect(
    () => () => {
      if (timer.current) window.clearTimeout(timer.current);
    },
    [],
  );
  if (mode === "collapsed") return <>{children}</>;
  /* the mark row: the nav's top padding, the 28px mark and its 8px gap, so the
     panel's first row lands where the rail's avatar sits */
  /* with the mark in the window bar the rail's first row is the avatar,
     and the panel's first row (8px in) lands on it from 4px down */
  const belowMark = mark ? "calc(0.75rem + 1.75rem + 0.5rem)" : "0.25rem";
  const shown = mode === "expanded" || open;
  const hover = mode === "hover";
  return (
    <div
      ref={setPortal}
      className="relative h-full"
      onPointerEnter={(e) =>
        hover && e.pointerType === "mouse" && later(true, OPEN_AFTER)
      }
      onPointerLeave={() => hover && later(false, CLOSE_AFTER)}
      /* Do not replace the hit target between pointerdown and pointerup.
         Pressing a rail icon focuses it, and opening on focus put a flyout
         row — aligned to the rail's own grid, so nothing looks like it
         moved — over the icon before the finger came up. The browser saw
         mousedown and mouseup on different elements and dispatched no
         click: the icon simply did not respond. A press holds the timer,
         a mouse release goes back to hover timing, and only real keyboard
         focus still opens at once. Ported from gents 7058d8b24. */
      onPointerDown={() => {
        if (timer.current) window.clearTimeout(timer.current);
      }}
      onPointerUp={(e) => hover && e.pointerType === "mouse" && later(true, OPEN_AFTER)}
      onFocus={(e) => hover && e.target.matches(":focus-visible") && later(true, 0)}
      onBlur={(e) => {
        if (hover && !e.currentTarget.contains(e.relatedTarget as Node | null))
          later(false, 0);
      }}
      onKeyDown={(e) => hover && e.key === "Escape" && setOpen(false)}
    >
      <PortalContainerProvider value={portal}>
        <FlyoutOpenContext.Provider value={hover && open}>
          {hover && children}
        </FlyoutOpenContext.Provider>
        {!hover && mark && (
          <div
            className="flex flex-col items-center"
            style={{ paddingTop: "0.75rem", width: "3.5rem" }}
          >
            <div className="mb-2">{mark}</div>
          </div>
        )}
        <div
          aria-hidden={!shown}
          style={{ top: belowMark }}
          className={cn(
            "flex w-72 flex-col gap-2 overflow-hidden bg-raised pt-2 pb-2",
            /* the flyout sits exactly where the expanded panel sits, so
               switching modes moves nothing but whether it stays */
            "rounded-2xl border border-border/60",
            hover
              ? "absolute bottom-2 left-2 z-30 shadow-lg transition-[opacity,transform] duration-200 ease-out"
              : "absolute bottom-2 left-2 shadow-xs",
            hover &&
              (open
                ? "translate-x-0 opacity-100"
                : "pointer-events-none -translate-x-2 opacity-0"),
          )}
        >
          <NavPanel
            route={route}
            agentName={agentName}
            agentDid={agentDid}
            deployment={deployment}
            online={online}
            mailboxCount={mailboxCount}
            holds={holds}
            settings={settings}
            foot={foot}
            recent={recent}
            working={working}
            nodeCount={nodeCount}
          />
        </div>
      </PortalContainerProvider>
    </div>
  );
}
