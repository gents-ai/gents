import { createContext } from "react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronRight, Inbox, Lock, Plus, ScrollText, Waypoints } from "lucide-react";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import { PortalContainerProvider } from "@gents/ui/lib/portal-container";
import { isLocalNode } from "@/lib/firstRun";
import { href, type Route } from "@/lib/router";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import type { NavMode } from "@/preferences";
import { NodeAvatar } from "@/screens/AgentAvatar";
import { ListedSessionStatus } from "@/screens/SessionStatus";
import { SyncHealth } from "./SyncHealth";
import { RecentFilter } from "./RecentFilter";
import { listViews, useListViews } from "./listViews";
import { emptyFilter, hasFilter } from "@/lib/session-filter";
import { navRowState, useNavItems, type NavContext, type NavItem } from "./navRegistry";
import { ContribBoundary } from "@/contrib/react/boundary";
import {
  useMailboxCount,
  useNodeCount,
  useOnline,
  usePinnedSessionList,
  useRecentCandidates,
  useRecentSessions,
  useSelectedNode,
  useSyncHealth,
  useHomeDid,
} from "@/hooks/useClient";

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

export function NavPanel({
  route,
  settings,
  syncRow,
  railAligned = false,
}: {
  route: Route;
  settings: ReactNode;
  syncRow: boolean;
  /** beside the rail, over it or pinned in its place: each row's icon
      sits at the rail's height for it */
  railAligned?: boolean;
}) {
  const online = useOnline();
  const mailboxCount = useMailboxCount();
  const pinned = usePinnedSessionList();
  const recent = useRecentSessions(RECENT);
  const recentCandidates = useRecentCandidates();
  const recentFiltered = useListViews((views) => hasFilter(views.recentFilter));
  const homeDid = useHomeDid();
  const nodeCount = useNodeCount();
  const syncHealth = useSyncHealth();
  const selected = useSelectedNode();
  const primary = useNavItems("primary");
  const footer = useNavItems("footer");
  const navContext: NavContext = {
    mailboxCount,
    selectedNodeDid: selected?.nodeDid ?? null,
  };
  /* a row a UX plugin contributed, drawn like the app's own; its
     predicates are evaluated apart and its icon drawn behind a boundary,
     so one bad row costs that row; a row with nowhere to go right now is
     not drawn */
  const contributed = (item: NavItem) => {
    const { to, active, count } = navRowState(item, route, navContext);
    if (!to) return null;
    return (
      <ContribBoundary key={item.id} id={item.id} variant="chip">
        <Item to={href(to)} active={active} count={count} icon={item.icon}>
          {item.label}
        </Item>
      </ContribBoundary>
    );
  };
  const currentSession = route.name === "session" ? route.sessionId : null;
  const selectedCurrent =
    selected !== null && route.name === "agent" && route.nodeDid === selected.nodeDid;
  return (
    <>
      {selected ? (
        <a
          href={href({
            name: "agent",
            nodeDid: selected.nodeDid,
            section: "agent",
          })}
          aria-current={selectedCurrent ? "page" : undefined}
          title={`${selected.node.displayName ?? selected.label} · ${selected.nodeDid}`}
          data-testid="working-node"
          className={cn(
            "group mr-3 ml-[14px] flex h-9 items-center gap-3 rounded-lg text-sm transition-colors",
            railAligned ? "mb-1" : "mb-2",
            selectedCurrent && "text-ink",
          )}
        >
          <span
            className={cn(
              "relative block size-7 shrink-0 rounded-full ring-1 ring-border ring-offset-2 ring-offset-raised transition-shadow group-hover:ring-muted-foreground",
              selectedCurrent && "ring-2 ring-ink",
            )}
          >
            <NodeAvatar
              name={selected.node.displayName ?? selected.label}
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
              {selected.node.displayName ?? selected.label}
            </span>
            <span className="block text-xs text-muted-foreground">
              {isLocalNode(selected, homeDid) ? "Local node" : "Remote node"} ·{" "}
              {selected.nodeDid.slice(-8)}
            </span>
          </span>
        </a>
      ) : (
        <a
          href={href({ name: "nodes" })}
          data-testid="no-working-node"
          className={cn(
            "group mr-3 ml-[14px] flex h-9 items-center gap-3 rounded-lg text-sm text-muted-foreground transition-colors hover:text-foreground",
            railAligned ? "mb-1" : "mb-2",
          )}
        >
          <span className="grid size-7 shrink-0 place-items-center rounded-full ring-1 ring-dashed ring-border">
            <Waypoints className="size-3.5" />
          </span>
          <span className="min-w-0 flex-1">
            <span className="block truncate font-heading font-medium">
              No active node
            </span>
            <span className="block text-xs">Choose a node to continue</span>
          </span>
        </a>
      )}
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
        active={
          route.name === "sessions" ||
          route.name === "sessionsUnder" ||
          route.name === "sessionsArchived"
        }
        icon={<ScrollText className="size-4" />}
      >
        Sessions
      </Item>
      {primary.map(contributed)}
      {pinned.length > 0 && (
        <>
          <p className="mt-2 mb-0 px-5 font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
            Pinned
          </p>
          <ul className="px-3">
            {pinned.map((c) => (
              <SessionLink key={c.sessionId} session={c} current={currentSession} />
            ))}
          </ul>
        </>
      )}
      {(recent.length > 0 || recentFiltered) && (
        <>
          <div className="mt-2 flex items-center gap-1 pr-3 pl-5">
            <p className="m-0 min-w-0 flex-1 font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
              Recent sessions
            </p>
            <RecentFilter sessions={recentCandidates} />
          </div>
          <ScrollArea className="min-h-0">
            <ul className="px-3">
              {recent.map((c) => (
                <SessionLink key={c.sessionId} session={c} current={currentSession} />
              ))}
            </ul>
            {recent.length === 0 && (
              <p className="m-0 px-5 py-1 text-xs text-muted-foreground">
                No sessions match.{" "}
                <button
                  type="button"
                  className="cursor-pointer underline hover:text-foreground"
                  onClick={() => listViews.setRecentFilter(emptyFilter)}
                >
                  Clear filters
                </button>
              </p>
            )}
          </ScrollArea>
          <a
            href={href({ name: "sessions" })}
            className="flex items-center gap-1 px-5 py-1 text-xs text-muted-foreground hover:text-foreground"
          >
            All sessions <ChevronRight className="size-3" />
          </a>
        </>
      )}
      <div className="mt-auto flex flex-col gap-1">
        {syncRow && (
          <div className="mx-3">
            <SyncHealth syncHealth={syncHealth} row />
          </div>
        )}
        <div className="mx-3 h-px bg-border" />
        {footer.map(contributed)}
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

export const FlyoutOpenContext = createContext(false);

export function RailFlyout({
  route,
  mode = "hover",
  settings,
  children,
}: {
  route: Route;
  mode?: NavMode;
  settings: ReactNode;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
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

        <div
          aria-hidden={!shown}
          className={cn(
            "absolute top-1 bottom-2 flex w-72 flex-col gap-2 overflow-hidden bg-raised pb-2",
            /* over the rail, its icons stay where the rail's were */
            hover ? "-left-px rounded-l-none" : "left-2",
            "pt-3.5",
            "rounded-2xl border border-border/60",
            hover
              ? "z-30 shadow-lg transition-[opacity,transform] duration-200 ease-out"
              : "shadow-xs",
            hover &&
              (open
                ? "translate-x-0 opacity-100"
                : "pointer-events-none -translate-x-2 opacity-0"),
          )}
        >
          <NavPanel route={route} settings={settings} syncRow={!hover} railAligned />
        </div>
      </PortalContainerProvider>
    </div>
  );
}

function SessionLink({
  session: c,
  current,
}: {
  session: SessionSummary;
  current: string | null;
}) {
  return (
    <li>
      <a
        href={href({ name: "session", sessionId: c.sessionId, nodeDid: c.nodeDid })}
        aria-current={current === c.sessionId ? "page" : undefined}
        className={cn(
          "flex h-8 items-center gap-2.5 rounded-lg px-2 text-[13px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
          current === c.sessionId && "text-foreground",
        )}
      >
        <ListedSessionStatus session={c} />
        <span className="min-w-0 flex-1 truncate">{c.title ?? "Untitled session"}</span>
        {c.unreadableReason && (
          <Lock className="size-3 shrink-0" role="img" aria-label={c.unreadableReason}>
            <title>{c.unreadableReason}</title>
          </Lock>
        )}
      </a>
    </li>
  );
}
