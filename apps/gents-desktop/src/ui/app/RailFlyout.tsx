import { createContext } from "react";
import { Fragment, useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronRight, CornerDownRight, Lock, Waypoints } from "lucide-react";
import { cn } from "@gents/ui/lib/utils";
import { PortalContainerProvider } from "@gents/ui/lib/portal-container";
import { isLocalNode } from "@/lib/firstRun";
import { href, type Route } from "@/lib/router";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import type { NavMode } from "@/preferences";
import { NodeAvatar, nodeAvatarOf } from "@/screens/AgentAvatar";
import { ListedSessionStatus } from "@/screens/SessionStatus";
import { Hint } from "@/screens/Hint";
import { parentOf, useFleet } from "@/hooks/useFleet";
import { SyncHealth } from "./SyncHealth";
import { NodeSwitcher } from "./NodeSwitcher";
import { RecentFilter } from "./RecentFilter";
import { listViews, recentDefault, useListViews } from "./listViews";
import { sameFilter } from "@/lib/session-filter";
import { navRowState, type NavContext } from "./navRegistry";
import {
  drawNavRows,
  mailboxGroupOf,
  useNavFooter,
  useNavRows,
  useNavSections,
  type NavRow,
  type NavSection,
} from "./navLayout";
import { useMailboxList } from "@/screens/MailboxStack";
import { NavMenu } from "./CustomizeNav";
import { NavScroll } from "./NavScroll";
import { NavMailbox } from "./NavMailbox";
import { NavNodes } from "./NavNodes";
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

/* a row's key, faint, once the pointer rests on it or it has focus; a sweep
   down the nav shows none */
export function KeyHint({ keys }: { keys?: string }) {
  if (!keys) return null;
  return (
    <span
      aria-hidden
      className="shrink-0 font-mono text-[11px] text-muted-foreground opacity-0 transition-opacity delay-0 group-hover/item:opacity-100 group-hover/item:delay-500 group-focus-visible/item:opacity-100"
    >
      {keys}
    </span>
  );
}

function Item({
  to,
  active,
  count,
  tally,
  icon,
  keys,
  children,
}: {
  to: string;
  active?: boolean;
  /** what waits for the person here, in the brand badge */
  count?: number;
  /** how many there are here, a plain fact, in quiet text */
  tally?: number;
  icon: ReactNode;
  /** the key that goes here, named on hover */
  keys?: string;
  children: ReactNode;
}) {
  return (
    <a
      href={to}
      aria-current={active ? "page" : undefined}
      className={cn(
        "group/item mx-3 flex h-8 items-center gap-2 rounded-lg border border-transparent pr-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
        active && "border-border/60 bg-background text-ink shadow-xs",
        /* on the window's ground the page colour wouldn't show; the rail's
           fill alone marks the page there */
        active &&
          "in-data-[ground=window]:border-transparent in-data-[ground=window]:bg-raised in-data-[ground=window]:shadow-none",
      )}
    >
      <span className="grid size-[30px] shrink-0 place-items-center">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{children}</span>
      <KeyHint keys={keys} />
      {tally ? (
        <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">
          {tally}
        </span>
      ) : null}
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
  const { sections, recent: recentCount, listed } = useNavSections();
  const { groups: mailboxGroups } = useMailboxList("");
  const recent = useRecentSessions(recentCount);
  const recentCandidates = useRecentCandidates();
  const recentFiltered = useListViews(
    (views) => !sameFilter(views.recentFilter, recentDefault),
  );
  const homeDid = useHomeDid();
  const nodeCount = useNodeCount();
  const syncHealth = useSyncHealth();
  const selected = useSelectedNode();
  const rows = useNavRows();
  const footer = useNavFooter();
  const navContext: NavContext = {
    mailboxCount,
    selectedNodeDid: selected?.nodeDid ?? null,
  };
  /* a row a UX plugin contributed is drawn like the app's own; its
     predicates are evaluated apart and its icon drawn behind a boundary,
     so one bad row costs that row; a row with nowhere to go right now is
     not drawn */
  const drawRow = (item: NavRow) => {
    const { to, active, count } = navRowState(item, route, navContext);
    if (!to) return null;
    const drawn = (
      <Item
        key={item.id}
        to={href(to)}
        keys={item.keys}
        active={active}
        count={count}
        /* one node, the person's own, is no news */
        tally={item.countsNodes && nodeCount > 1 ? nodeCount : undefined}
        icon={item.icon}
      >
        {item.label}
      </Item>
    );
    if (item.own) return drawn;
    return (
      <ContribBoundary key={item.id} id={item.id} variant="chip">
        {drawn}
      </ContribBoundary>
    );
  };
  const currentSession = route.name === "session" ? route.sessionId : null;
  /* a section, or nothing while it has nothing to list, so a divider
     beside it isn't drawn either */
  const drawSection = (section: NavSection): ReactNode => {
    const group = mailboxGroupOf(section.id);
    if (group) {
      /* every item, a decision before what went wrong before what's worth
         a look before what's done, so the few shown are those that most
         need the person */
      const found =
        group === "all"
          ? {
              key: group,
              label: "Mailbox",
              items: mailboxGroups.flatMap((g) => g.items),
            }
          : mailboxGroups.find((g) => g.key === group);
      return found && found.items.length > 0 ? (
        <NavMailbox key={section.id} group={found} limit={listed(section.id)} />
      ) : null;
    }
    if (section.id === "node-list")
      return nodeCount > 1 ? (
        <NavNodes key="node-list" route={route} current={selected?.nodeDid ?? null} />
      ) : null;
    if (section.id === "pinned")
      return pinned.length > 0 ? (
        <Fragment key="pinned">
          <p className="mt-2 mb-0 px-5 font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
            Pinned
          </p>
          <ul className="px-3">
            {pinned.map((c) => (
              <SessionLink key={c.sessionId} session={c} current={currentSession} />
            ))}
          </ul>
        </Fragment>
      ) : null;
    return recent.length > 0 || recentFiltered ? (
      <Fragment key="recent">
        <div className="mt-2 flex items-center gap-1 pr-3 pl-5">
          <p className="m-0 min-w-0 flex-1 font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
            Recent
          </p>
          <RecentFilter sessions={recentCandidates} />
        </div>
        <div>
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
                onClick={() => listViews.setRecentFilter(recentDefault)}
              >
                Reset filters
              </button>
            </p>
          )}
        </div>
        <a
          href={href({ name: "sessions" })}
          className="flex items-center gap-1 px-5 py-1 text-xs text-muted-foreground hover:text-foreground"
        >
          All sessions <ChevronRight className="size-3" />
        </a>
      </Fragment>
    ) : null;
  };
  const selectedCurrent =
    selected !== null && route.name === "agent" && route.nodeDid === selected.nodeDid;
  return (
    <>
      {selected ? (
        /* the node goes to its page; the caret beside it switches node */
        <div
          className={cn(
            "mr-3 ml-[14px] flex items-center gap-1",
            railAligned ? "mb-1" : "mb-2",
          )}
        >
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
              "group flex h-9 min-w-0 flex-1 items-center gap-3 rounded-lg text-sm transition-colors",
              selectedCurrent && "text-ink",
            )}
          >
            <span
              className={cn(
                "relative block size-7 shrink-0 rounded-full ring-1 ring-border ring-offset-2 ring-offset-[color:var(--nav-ground,var(--color-raised))] transition-shadow group-hover:ring-muted-foreground",
                selectedCurrent && "ring-2 ring-ink",
              )}
            >
              <NodeAvatar
                name={selected.node.displayName ?? selected.label}
                {...nodeAvatarOf(selected)}
                className="size-7"
              />
              <span
                className={cn(
                  "absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-[color:var(--nav-ground,var(--color-raised))]",
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
          <NodeSwitcher route={route} current={selected.nodeDid} />
        </div>
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
      {/* the top items and the sections scroll together when the window
          is too short for them, and the foot stays */}
      <NavScroll>
        <div className="flex flex-col gap-2 py-1.5">
          {drawNavRows(rows, drawRow, (id) => (
            <div
              key={id}
              data-testid="nav-divider"
              className="mx-3 my-1 h-px bg-border"
            />
          ))}
          {drawNavRows(sections, drawSection, (id) => (
            <div
              key={id}
              data-testid="nav-divider"
              className="mx-5 my-1 h-px bg-border"
            />
          ))}
        </div>
      </NavScroll>
      <div className="flex shrink-0 flex-col gap-1">
        {footer.status &&
          (syncRow ? (
            <div className="mx-3">
              <SyncHealth syncHealth={syncHealth} row />
            </div>
          ) : (
            /* on hover the rail's status dot grows into this row over the
               panel; its place is kept so nothing scrolls under it */
            railAligned && <div aria-hidden className="h-8 shrink-0" />
          ))}
        <div className="mx-3 h-px bg-border" />
        {footer.rows.map(drawRow)}
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
  const pointerInside = useRef(false);
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
      onPointerEnter={(e) => {
        pointerInside.current = true;
        if (hover && e.pointerType === "mouse") later(true, OPEN_AFTER);
      }}
      onPointerLeave={() => {
        pointerInside.current = false;
        if (hover) later(false, CLOSE_AFTER);
      }}
      onPointerDown={() => {
        if (timer.current) window.clearTimeout(timer.current);
      }}
      onPointerUp={(e) => hover && e.pointerType === "mouse" && later(true, OPEN_AFTER)}
      onFocus={(e) => hover && e.target.matches(":focus-visible") && later(true, 0)}
      onBlur={(e) => {
        /* focus leaving closes it for the keyboard; a page that focuses its
           own field, as a session's composer does, mustn't close it under
           the pointer */
        if (
          hover &&
          !pointerInside.current &&
          !e.currentTarget.contains(e.relatedTarget as Node | null)
        )
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
          data-ground={hover ? "raised" : "window"}
          className={cn(
            "absolute top-1 bottom-2 flex w-72 flex-col gap-2 overflow-hidden pb-2",
            "left-1.5 pt-3.5",
            /* floating over the page it is a surface of its own; always there,
               it is part of the window, on the window's ground */
            hover
              ? "z-30 rounded-2xl border border-border/60 bg-(--nav-ground) shadow-lg transition-opacity duration-200 ease-out [--nav-ground:var(--color-raised)] dark:[--nav-ground:color-mix(in_oklab,var(--color-raised),var(--color-surface))]"
              : "[--nav-ground:var(--color-background)]",
            hover &&
              /* a fade, not a slide: its icons sit over the rail's and must
                 not move while it opens or closes */
              (open ? "opacity-100" : "pointer-events-none opacity-0"),
          )}
        >
          {/* the panel keeps its margin from the window's edge, and its
              content moves left by the rest, so each icon sits where the
              rail's does */}
          <NavMenu
            render={<div className="-mx-[7px] flex min-h-0 flex-1 flex-col gap-1" />}
          >
            <NavPanel route={route} settings={settings} syncRow={!hover} railAligned />
          </NavMenu>
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
  const starter = useFleet((s) => (c.startedBy ? parentOf(s, c) : null));
  /* a sub-session, shown in a list ordered by time: marked, not nested */
  const startedBy = c.startedBy
    ? `Started by ${starter ? (starter.title ?? "Untitled session") : "another session"}`
    : null;
  const link = (
    <a
      href={href({ name: "session", sessionId: c.sessionId, nodeDid: c.nodeDid })}
      aria-current={current === c.sessionId ? "page" : undefined}
      className={cn(
        "flex h-8 items-center gap-2.5 rounded-lg px-2 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
        current === c.sessionId && "text-foreground",
      )}
    >
      <ListedSessionStatus session={c} />
      <span className="flex min-w-0 flex-1 items-center gap-1.5">
        {startedBy && (
          <CornerDownRight
            className="size-3 shrink-0 text-muted-foreground/70"
            role="img"
            aria-label={startedBy}
          />
        )}
        <span className="min-w-0 truncate">{c.title ?? "Untitled session"}</span>
      </span>
      {c.unreadableReason && (
        <Lock className="size-3 shrink-0" role="img" aria-label={c.unreadableReason}>
          <title>{c.unreadableReason}</title>
        </Lock>
      )}
    </a>
  );
  return <li>{startedBy ? <Hint label={startedBy}>{link}</Hint> : link}</li>;
}
