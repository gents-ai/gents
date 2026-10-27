import {
  Fragment,
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useState,
  type ComponentProps,
  type CSSProperties,
} from "react";
import { toast } from "sonner";
import { useShallow } from "zustand/react/shallow";
import {
  ChevronsDownUp,
  ChevronsUpDown,
  CornerDownRight,
  Archive,
  Ellipsis,
  Lock,
  MessageSquare,
  Pin,
  Play,
  Plus,
  Search,
  ChevronRight,
  X,
} from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import {
  Breadcrumb,
  BreadcrumbEllipsis,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@gents/ui/components/breadcrumb";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { href, navigate } from "@/lib/router";
import { useListWindow } from "@source-inc/gents-desktop-ui";
import { Age } from "./time";
import {
  defaultScope,
  fleetNodes,
  knownNodeIds,
  nodesInScope,
  sessionsInScope,
  type Scope,
} from "@/lib/scope";
import { useHomeDid, useInScope, useSelectedNodeDid } from "@/hooks/useClient";
import { sessionKeyOf } from "../../hooks/fleetStore";
import { nodeDidOf, nodeOfSession } from "@/lib/nodes";
import { NodeAgentStack } from "./NodeBehaviorStack";
import { NodeAxis } from "./NodeAxis";
import { SessionStatus } from "./SessionStatus";
import { StartedCount } from "./StartedCount";
import { SessionFilters, matchesView } from "./SessionFilters";
import { emptyFilter, hasFilter } from "@/lib/session-filter";
import { sessionRows, type SessionRow as Row } from "@/lib/session-rows";
import {
  REVEAL_EVENT,
  SessionRowActions,
  SessionRowMenu,
  takeReveal,
} from "./SessionRowMenu";
import { Tooltip, TooltipContent, TooltipTrigger } from "@gents/ui/components/tooltip";
import { listViews, useListViews } from "@/app/listViews";
import { useApp } from "@/app/AppContext";
import { usePinnedSessions } from "@/preferences";
import { NO_SESSIONS, useFleet } from "../hooks/useFleet";
import { RenameField } from "./SessionHeader";

/* a session, and after an open group that leaves some out, the way to all
   of them */
type Item = { kind: "session" | "more"; row: Row };
const itemKey = ({ kind, row }: Item) =>
  kind === "session" ? sessionKeyOf(row.session) : `${sessionKeyOf(row.session)}-more`;
const itemHeight = ({ kind, row }: Item) =>
  kind === "more" ? 33 : row.depth ? 41 : 61 + (row.path?.length ? 16 : 0);
/* a long group arrives as fast as a short one */
const ENTRANCE_STEPS = 4;
const NONE_FRESH: ReadonlySet<string> = new Set();
/* where an opened row's status sits: each depth one step in */
const STEP = 24;
const titleOf = (session: SessionSummary) => session.title ?? "Untitled";
const isArchived = (session: SessionSummary) => session.archivedAt !== null;

export function SessionsScreen({
  nodeDid,
  under,
  archived = false,
  entry,
}: {
  nodeDid?: string;
  /** the session whose started sessions the list shows */
  under?: string;
  /** list the archived sessions instead of the rest */
  archived?: boolean;
  /** the history entry showing the list, which keeps the rows opened on it */
  entry?: object;
}) {
  const nodes = useFleet(useShallow(fleetNodes));
  const homeDid = useHomeDid();
  const selectedNodeDid = useSelectedNodeDid();
  const [query, setQuery] = useState<string | null>(null);
  const filter = useListViews((v) => v.sessionFilter);
  const opened = useOpened(entry);

  const placement = { nodes, selectedNodeDid, homeDid };
  const defaultNodeIds = nodesInScope(defaultScope("sessions"), placement).map(
    nodeDidOf,
  );
  const storedNodeIds = useListViews((v) => v.sessionNodes);
  useLayoutEffect(() => {
    if (nodeDid) listViews.setSessionNodes([nodeDid]);
  }, [nodeDid]);

  const knownIds = storedNodeIds && knownNodeIds(storedNodeIds, placement);
  const nodeIds =
    !knownIds || (knownIds.length === 0 && storedNodeIds!.length > 0)
      ? defaultNodeIds
      : knownIds;

  const shownNodeIds = nodeIds.length ? nodeIds : nodes.map(nodeDidOf);
  const nodesPicked =
    shownNodeIds.length !== defaultNodeIds.length ||
    shownNodeIds.some((id) => !defaultNodeIds.includes(id));
  const scope: Scope = { nodes: nodeIds.length ? nodeIds : "all", agents: [] };
  const scoped = useInScope((ctx) => sessionsInScope(scope, ctx));
  /* archived sessions leave the list for their own; the runtime still
     lists them */
  const inScope = useMemo(
    () =>
      archived
        ? scoped.filter(isArchived)
        : scoped.some(isArchived)
          ? scoped.filter((c) => !isArchived(c))
          : scoped,
    [scoped, archived],
  );
  const nodeCounts = useFleet(
    useShallow((s) =>
      Object.fromEntries(
        s.nodeKeys.map((key) => [
          key,
          s.sessionsOf[key]?.filter((c) => !isArchived(c)).length ?? 0,
        ]),
      ),
    ),
  );
  const pinnedKeys = usePinnedSessions();
  const pinned = useMemo(() => new Set(pinnedKeys), [pinnedKeys]);
  const parents = useFleet((s) => s.parentOf);
  const parentOf = useCallback(
    (c: SessionSummary): SessionSummary | null => parents[sessionKeyOf(c)] ?? null,
    [parents],
  );
  /* a session sits under the nearest starter the list shows, through the
     ones it leaves out (another node, a narrowed pick) */
  const listedParents = useMemo(() => {
    const listed = new Set(inScope.map(sessionKeyOf));
    const nearest = new Map<string, SessionSummary | null>();
    for (const c of inScope) {
      const seen = new Set([sessionKeyOf(c)]);
      let parent = parentOf(c);
      while (parent && !listed.has(sessionKeyOf(parent))) {
        if (seen.has(sessionKeyOf(parent))) {
          parent = null;
          break;
        }
        seen.add(sessionKeyOf(parent));
        parent = parentOf(parent);
      }
      nearest.set(sessionKeyOf(c), parent);
    }
    return nearest;
  }, [inScope, parentOf]);
  const filtering = hasFilter(filter) || Boolean(query);
  const { rows, trail, shown, total } = useMemo(
    () =>
      sessionRows({
        sessions: inScope,
        parentOf: (c) => listedParents.get(sessionKeyOf(c)) ?? null,
        under: under ?? null,
        matches: filtering ? (c) => matchesView(c, filter, query) : null,
        isOpen: (key) => opened.keys.has(key),
        pinned,
      }),
    [inScope, listedParents, under, filter, query, filtering, opened.keys, pinned],
  );
  const level = trail[trail.length - 1] ?? null;
  const listed = useMemo(
    () => new Map(inScope.map((c) => [sessionKeyOf(c), c])),
    [inScope],
  );

  const narrowed =
    filtering || (!archived && Object.values(nodeCounts).some((count) => count > 0));
  const pickedNames = nodesPicked
    ? nodes
        .filter((n) => nodeIds.includes(nodeDidOf(n)))
        .map((n) => n.node.displayName ?? n.label)
    : [];
  const leaveNode = () => {
    if (nodeDid) navigate({ name: "sessions" });
  };

  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  const scrollRoot = useCallback((root: HTMLDivElement | null) => {
    setScroller(
      root?.querySelector<HTMLElement>('[data-slot="scroll-area-viewport"]') ?? null,
    );
  }, []);
  return (
    <ScrollArea ref={scrollRoot} className="h-full" data-testid="sessions-screen">
      {/* measured, so a row's toggle knows whether the margin has room */}
      <div className="@container">
        <div className="mx-auto max-w-page px-6 py-6">
          <div className="flex h-10 items-center gap-2">
            {archived ? (
              <h1 className="flex items-center gap-1.5 font-heading text-lg font-medium text-heading">
                <a
                  href={href({ name: "sessions" })}
                  className="text-muted-foreground hover:text-foreground"
                >
                  Sessions
                </a>
                <ChevronRight className="size-4 text-muted-foreground" />
                Archived
              </h1>
            ) : trail.length === 0 ? (
              <h1 className="font-heading text-lg font-medium text-heading">
                Sessions
              </h1>
            ) : (
              <SessionTrail trail={trail} />
            )}
            <div className="ml-auto flex shrink-0 items-center gap-2">
              <NodeAxis
                nodes={nodes}
                homeDid={homeDid}
                counts={nodeCounts}
                /* every node is the picker's open state, "All nodes", however
                 the list came to show them all */
                value={nodesPicked ? nodeIds : []}
                onChange={(next) => {
                  listViews.setSessionNodes(next);
                  leaveNode();
                }}
              />
              <SessionFilters
                sessions={inScope}
                nodeDids={nodesInScope(scope, placement).map(nodeDidOf)}
                value={filter}
                onChange={listViews.setSessionFilter}
              />
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Search sessions"
                aria-expanded={query !== null}
                className={cn("shrink-0", query !== null && "text-foreground")}
                onClick={() => setQuery(query === null ? "" : null)}
              >
                <Search />
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger
                  render={
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label="More session views"
                      className="shrink-0"
                    />
                  }
                >
                  <Ellipsis />
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-48">
                  <DropdownMenuGroup>
                    <DropdownMenuItem
                      render={
                        <a
                          href={href(
                            archived
                              ? { name: "sessions" }
                              : { name: "sessionsArchived" },
                          )}
                        />
                      }
                    >
                      <Archive />
                      {archived ? "View sessions" : "View archived"}
                    </DropdownMenuItem>
                  </DropdownMenuGroup>
                </DropdownMenuContent>
              </DropdownMenu>
              <Button
                variant="brand"
                size="sm"
                className="shrink-0"
                nativeButton={false}
                render={<a href={href({ name: "session", sessionId: null })} />}
              >
                <Plus className="sm:hidden" />
                <span className="max-sm:sr-only">New</span>
              </Button>
            </div>
          </div>
          {query !== null && (
            <div className="relative mt-3">
              <Input
                autoFocus
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => e.key === "Escape" && setQuery(null)}
                placeholder={
                  level ? `Search under ${titleOf(level)}` : "Search sessions"
                }
                className="h-9 w-full pr-9"
                aria-label="Search sessions"
              />
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Clear search"
                className="absolute top-1/2 right-1 -translate-y-1/2 text-muted-foreground"
                onClick={() => setQuery(null)}
              >
                <X />
              </Button>
            </div>
          )}
          {/* a narrowed list says so, whatever narrowed it and whenever, a
            filter restored from the last visit included; the region stays
            mounted so a change in it is announced */}
          <div role="status">
            {(filtering || nodesPicked) && (
              <div
                data-testid="sessions-narrowed"
                className="mt-2 flex items-center justify-end gap-2 text-xs text-muted-foreground"
              >
                <span className="min-w-0 truncate">
                  {filtering
                    ? `${shown} of ${total} session${total === 1 ? "" : "s"}`
                    : `${inScope.length} session${inScope.length === 1 ? "" : "s"}`}
                  {nodesPicked &&
                    (pickedNames.length
                      ? ` on ${pickedNames.join(", ")}`
                      : ` on ${nodeIds.length} node${nodeIds.length === 1 ? "" : "s"}`)}
                </span>
                <Button
                  variant="quiet"
                  size="sm"
                  className="h-6 shrink-0 px-2 text-xs"
                  aria-label="Clear filters"
                  onClick={() => {
                    listViews.setSessionFilter(emptyFilter);
                    listViews.setSessionNodes(null);
                    setQuery(null);
                    leaveNode();
                  }}
                >
                  Clear
                </Button>
              </div>
            )}
          </div>
          <SessionList
            rows={rows}
            archived={archived}
            level={level}
            narrowed={narrowed}
            scroller={scroller}
            parentOf={parentOf}
            listed={listed}
            onToggle={opened.toggle}
          />
        </div>
      </div>
    </ScrollArea>
  );
}

/* past this many levels the middle folds into a menu, keeping the way
   home and the session shown */
const TRAIL_SHOWN = 1;

function SessionTrail({ trail }: { trail: readonly SessionSummary[] }) {
  const folded = trail.length > TRAIL_SHOWN ? trail.slice(0, -TRAIL_SHOWN) : [];
  const shown = trail.slice(folded.length);
  const crumb = "truncate rounded-md px-1.5 py-0.5";
  const under = (session: SessionSummary) =>
    href({ name: "sessionsUnder", session: sessionKeyOf(session) });
  return (
    <Breadcrumb className="-ml-1.5 min-w-0" data-testid="sessions-trail">
      <BreadcrumbList className="flex-nowrap gap-0.5 sm:gap-0.5">
        <BreadcrumbItem className="shrink-0">
          <BreadcrumbLink
            className={cn(crumb, "hover:bg-accent")}
            href={href({ name: "sessions" })}
          >
            Sessions
          </BreadcrumbLink>
        </BreadcrumbItem>
        {folded.length > 0 && (
          <>
            <BreadcrumbSeparator className="[&>svg]:size-3" />
            <BreadcrumbItem className="shrink-0">
              <DropdownMenu>
                <DropdownMenuTrigger
                  aria-label={`${folded.length} more level${folded.length === 1 ? "" : "s"}`}
                  className="flex h-6 cursor-pointer items-center rounded-md px-1 hover:bg-accent hover:text-foreground"
                >
                  <BreadcrumbEllipsis className="size-4" />
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  <DropdownMenuGroup>
                    {folded.map((session) => (
                      <DropdownMenuItem
                        key={sessionKeyOf(session)}
                        nativeButton={false}
                        render={<a href={under(session)} />}
                      >
                        <span className="max-w-72 truncate">{titleOf(session)}</span>
                      </DropdownMenuItem>
                    ))}
                  </DropdownMenuGroup>
                </DropdownMenuContent>
              </DropdownMenu>
            </BreadcrumbItem>
          </>
        )}
        {shown.map((session, n) => (
          <Fragment key={sessionKeyOf(session)}>
            <BreadcrumbSeparator className="[&>svg]:size-3" />
            <BreadcrumbItem className="min-w-0">
              {n === shown.length - 1 ? (
                <BreadcrumbPage className={cn(crumb, "font-medium")}>
                  <h1 className="truncate">{titleOf(session)}</h1>
                </BreadcrumbPage>
              ) : (
                <BreadcrumbLink
                  className={cn(crumb, "max-w-48 hover:bg-accent")}
                  href={under(session)}
                >
                  {titleOf(session)}
                </BreadcrumbLink>
              )}
            </BreadcrumbItem>
          </Fragment>
        ))}
      </BreadcrumbList>
    </Breadcrumb>
  );
}

function SessionList({
  rows,
  archived,
  level,
  narrowed,
  scroller,
  parentOf,
  listed,
  onToggle,
}: {
  rows: Row[];
  archived: boolean;
  level: SessionSummary | null;
  narrowed: boolean;
  scroller: HTMLElement | null;
  parentOf: (session: SessionSummary) => SessionSummary | null;
  listed: ReadonlyMap<string, SessionSummary>;
  /** with the rows a close takes along: everything opened under it */
  onToggle: (key: string, under: readonly string[]) => void;
}) {
  const items = useMemo(() => {
    const out: Item[] = [];
    const groups: Row[] = [];
    const close = (depth: number) => {
      while (groups.length && groups[groups.length - 1]!.depth >= depth) {
        const group = groups.pop()!;
        if (group.hidden > 0) out.push({ kind: "more", row: group });
      }
    };
    for (const row of rows) {
      close(row.depth);
      out.push({ kind: "session", row });
      if (row.open) groups.push(row);
    }
    close(0);
    return out;
  }, [rows]);
  const window = useListWindow(items, scroller, itemKey, itemHeight);

  /* the rows a toggle just opened come out of their row: briefly, and
     only then, not when the list first shows or a scroll brings them back */
  const [fresh, setFresh] = useState<ReadonlySet<string>>(NONE_FRESH);
  useEffect(() => {
    if (fresh.size === 0) return;
    const done = setTimeout(() => setFresh(NONE_FRESH), 400);
    return () => clearTimeout(done);
  }, [fresh]);
  const entering = useMemo(() => {
    const steps = new Map<string, number>();
    const line: string[] = [];
    const counts = new Map<string, number>();
    for (const row of rows) {
      line.length = row.depth;
      const from = line[row.depth - 1];
      if (from && fresh.has(from)) {
        const n = counts.get(from) ?? 0;
        counts.set(from, n + 1);
        steps.set(sessionKeyOf(row.session), Math.min(n, ENTRANCE_STEPS));
      }
      line[row.depth] = sessionKeyOf(row.session);
    }
    return steps;
  }, [rows, fresh]);
  const empty = rows.length === 0;

  const [list, setList] = useState<HTMLUListElement | null>(null);
  const ownRef = window.listProps.ref;
  const listRef = useCallback(
    (el: HTMLUListElement | null) => {
      setList(el);
      if (typeof ownRef === "function") ownRef(el);
      else if (ownRef) (ownRef as { current: HTMLUListElement | null }).current = el;
    },
    [ownRef],
  );
  const [flash, setFlash] = useState<string | null>(null);
  useEffect(() => {
    const reveal = () => {
      const found = rows.find((row) => takeReveal(row.session.sessionId));
      if (!found) return;
      const id = found.session.sessionId;
      list
        ?.querySelector(`[data-testid="session-${CSS.escape(id)}"]`)
        ?.scrollIntoView({ block: "center", behavior: "smooth" });
      setFlash(id);
    };
    reveal();
    addEventListener(REVEAL_EVENT, reveal);
    return () => removeEventListener(REVEAL_EVENT, reveal);
  }, [rows, list]);
  useEffect(() => {
    if (!flash) return;
    const done = setTimeout(() => setFlash(null), 1600);
    return () => clearTimeout(done);
  }, [flash]);

  return (
    <ul
      {...window.listProps}
      ref={listRef}
      aria-label="Sessions"
      data-session-count={rows.length}
      className={cn(
        "mt-4",
        !window.enabled && "divide-y divide-border/60",
        /* without the margin's room, rows that can open keep a gutter
           inside their highlight for the toggle */
        rows.some((row) => row.started > 0 && !row.path) &&
          "[--gutter:24px] @min-[1000px]:[--gutter:0px]",
      )}
    >
      {window.items.map(({ index, start }) => {
        const item = items[index]!;
        const { row } = item;
        const attributes = window.attributes(index, start);
        if (attributes && index > 0) attributes.className = "border-t border-border/60";
        if (item.kind === "more") {
          return (
            <li key={itemKey(item)} {...attributes}>
              <a
                href={href({
                  name: "sessionsUnder",
                  session: sessionKeyOf(row.session),
                })}
                data-testid={`sessions-all-${row.session.sessionId}`}
                /* in line with the status dots of the group it ends, which sit
                   centred in their 16px status box */
                className="-mx-3 flex w-fit items-center gap-0.5 py-2 text-xs text-muted-foreground hover:text-foreground"
                style={{
                  paddingLeft: `calc(${12 + (row.depth + 1) * STEP + 4}px + var(--gutter, 0px))`,
                }}
              >
                View all {row.started}
                <ChevronRight className="size-3" />
              </a>
            </li>
          );
        }
        return (
          <SessionRow
            key={itemKey(item)}
            attributes={attributes}
            row={row}
            /* under a level its starter is the trail; a top row names the
               one the list leaves out */
            parent={level || row.path || row.depth ? null : parentOf(row.session)}
            starter={
              row.starter
                ? (listed.get(row.starter.key) ?? null)
                : parentOf(row.session)
            }
            onToggle={
              row.path
                ? null
                : () => {
                    const key = sessionKeyOf(row.session);
                    if (!row.open) setFresh(new Set([key]));
                    onToggle(key, openedUnder(rows, rows.indexOf(row)));
                  }
            }
            flash={flash === row.session.sessionId}
            enter={entering.get(sessionKeyOf(row.session)) ?? null}
          />
        );
      })}
      {empty && archived && !narrowed && (
        <li className="py-8 text-center text-sm text-muted-foreground">
          No archived sessions.
        </li>
      )}
      {empty && narrowed && (
        <li className="py-8 text-center text-sm text-muted-foreground">
          No sessions match.
        </li>
      )}
      {empty && !archived && !narrowed && level && (
        <li className="py-8 text-center text-sm text-muted-foreground">
          No sessions under this one.
        </li>
      )}
      {empty && !archived && !narrowed && !level && (
        <li className="grid min-h-[50vh] place-items-center animate-in fade-in-0 slide-in-from-bottom-2 duration-300 ease-out fill-mode-both motion-reduce:animate-none">
          <div className="text-center">
            <MessageSquare className="mx-auto size-6 text-muted-foreground" />
            <p className="mt-3 font-heading text-lg font-medium text-heading">
              No sessions yet
            </p>
            <Button
              variant="brand"
              className="mt-5"
              nativeButton={false}
              render={<a href={href({ name: "session", sessionId: null })} />}
            >
              <Plus /> New session
            </Button>
          </div>
        </li>
      )}
    </ul>
  );
}

const SessionRow = memo(function SessionRow({
  row,
  parent,
  starter,
  onToggle,
  flash,
  enter,
  attributes = {},
}: {
  attributes?: ComponentProps<"li">;
  row: Row;
  parent: SessionSummary | null;
  starter: SessionSummary | null;
  onToggle: (() => void) | null;
  flash: boolean;
  /** its place in a group just opened, for a short staggered entrance */
  enter: number | null;
}) {
  const { session, path, depth, started, running, open } = row;
  const { actions } = useApp();
  const [renaming, setRenaming] = useState(false);
  const startRename = useCallback(() => setRenaming(true), []);
  const condensed = depth > 0;
  const count =
    `${started} started session${started === 1 ? "" : "s"}` +
    (running > 0 ? ` · ${running} running` : "");
  return (
    <li
      {...attributes}
      data-depth={depth}
      className={cn(attributes.className, "group/row relative")}
    >
      <SessionRowMenu
        row={row}
        starter={starter}
        onToggle={onToggle}
        onRename={startRename}
        trigger={
          /* the title's link covers the row, so the count can be a link of
             its own; what has its own pointer behaviour sits above it */
          <div
            className={cn(
              "relative -mx-3 grid grid-cols-[auto_1fr_auto_auto_auto] items-center gap-4 rounded-lg px-3 transition-colors duration-500 hover:bg-accent data-popup-open:bg-accent",
              condensed ? "py-2" : "py-3.5",
              /* without hover the actions stay, in a column of their own
                 after the time */
              "[@media(hover:none)]:pr-11",
              flash && "bg-accent",
              enter !== null &&
                "animate-in fade-in-0 slide-in-from-top-1 fill-mode-both duration-150 ease-out motion-reduce:animate-none",
            )}
            style={{
              paddingLeft: `calc(${12 + depth * STEP}px + var(--gutter, 0px))`,
              animationDelay: enter ? `${enter * 15}ms` : undefined,
            }}
          />
        }
      >
        <SessionStatus turnState={session.turnState} />
        <div className="min-w-0">
          <div className="flex min-w-0 items-center gap-1.5 text-sm">
            {!parent && session.taskId && (
              <Play
                className="relative z-10 size-3.5 shrink-0 text-muted-foreground"
                role="img"
                aria-label={`Started by ${session.taskName ?? "a task"}`}
              >
                <title>
                  Started by {session.taskName ?? "a task"}
                  {session.triggerKind ? ` · ${session.triggerKind}` : ""}
                </title>
              </Play>
            )}
            {parent && (
              <CornerDownRight
                className="relative z-10 size-3.5 shrink-0 text-muted-foreground"
                role="img"
                aria-label={`Started by ${titleOf(parent)}`}
              >
                <title>Started by {titleOf(parent)}</title>
              </CornerDownRight>
            )}
            {session.unreadableReason && (
              <Lock
                className="relative z-10 size-3.5 shrink-0 text-muted-foreground"
                role="img"
                aria-label={session.unreadableReason}
              >
                <title>{session.unreadableReason}</title>
              </Lock>
            )}
            {renaming ? (
              <span className="min-w-0 flex-1">
                <RenameField
                  title={titleOf(session)}
                  onRename={async (title) => {
                    await actions.renameSession(
                      session.sessionId,
                      title,
                      session.nodeDid,
                    );
                    toast("Renamed");
                  }}
                  onDone={() => setRenaming(false)}
                  className="-my-1 h-7 w-full text-sm"
                />
              </span>
            ) : (
              <a
                href={href({
                  name: "session",
                  sessionId: session.sessionId,
                })}
                data-testid={`session-${session.sessionId}`}
                className={cn(
                  "truncate outline-none after:absolute after:inset-0 after:rounded-lg focus-visible:after:ring-2 focus-visible:after:ring-ring",
                  (condensed || session.unreadableReason) && "text-muted-foreground",
                )}
              >
                {titleOf(session)}
              </a>
            )}
            {row.pinned && !renaming && (
              <Pin
                className="relative z-10 size-3.5 shrink-0 text-muted-foreground"
                role="img"
                aria-label="Pinned"
              >
                <title>Pinned</title>
              </Pin>
            )}
          </div>
          {path && path.length > 0 && (
            <p className="truncate text-xs text-muted-foreground">
              in {path.map(titleOf).join(" › ")}
            </p>
          )}
        </div>
        {/* a column of its own, so counts line up down the list */}
        <span className="flex w-16 justify-end">
          <StartedCount session={session} started={started} running={running} />
        </span>
        {/* a slot that fits the agent and its node, so the counts before it
            line up down the list */}
        <span className="relative z-10 flex min-w-12 justify-end">
          <NodeAgentStack
            nodeDid={nodeOfSession(session)}
            agentId={session.agentId}
            size={condensed ? "sm" : "md"}
          />
        </span>
        <span
          className={cn(
            "w-10 text-right text-muted-foreground sm:w-14",
            condensed ? "text-xs" : "text-sm",
            /* gives its place to the actions while the row is pointed at */
            "group-hover/row:invisible [@media(hover:hover)]:group-has-[[data-popup-open]]/row:invisible",
          )}
        >
          <Age iso={session.updatedAt} />
        </span>
      </SessionRowMenu>
      <SessionRowActions
        row={row}
        starter={starter}
        onToggle={onToggle}
        onRename={startRename}
        className="absolute top-1/2 -right-1.5 -translate-y-1/2 opacity-0 group-hover/row:opacity-100 focus-visible:opacity-100 data-popup-open:opacity-100 [@media(hover:none)]:opacity-100"
      />
      {/* its tooltip goes as soon as the pointer leaves the toggle, so it
          never sits over the toggles of the rows around it */}
      {started > 0 && onToggle && (
        <Tooltip disableHoverablePopup>
          <TooltipTrigger
            render={
              <button
                type="button"
                aria-expanded={open}
                aria-label={open ? `Hide ${count}` : `Show ${count}`}
                data-testid={`sessions-toggle-${session.sessionId}`}
                /* an expand shows while the row is pointed at or the toggle
                   has focus; a screen without hover always shows it */
                className={cn(
                  "absolute top-1/2 grid size-5 -translate-y-1/2 cursor-pointer place-items-center rounded-md text-muted-foreground group-hover/row:opacity-100 hover:bg-background hover:text-foreground focus-visible:opacity-100 [@media(hover:none)]:opacity-100",
                  open ? "opacity-60" : "opacity-0",
                  /* in the page margin, clear of the highlight, at every depth,
                     once the centred page leaves it room (a 976px page with its
                     24px gutter needs 12px more on each side); before that, in
                     the row's own gutter before its status */
                  "left-(--indent) @min-[1000px]:-left-9",
                )}
                style={{ "--indent": `${depth * STEP - 4}px` } as CSSProperties}
                onClick={onToggle}
              />
            }
          >
            {open ? (
              <ChevronsDownUp className="size-3.5" />
            ) : (
              <ChevronsUpDown className="size-3.5" />
            )}
          </TooltipTrigger>
          <TooltipContent side="top" className="pointer-events-none">
            {open ? "Collapse" : "Expand"}
          </TooltipContent>
        </Tooltip>
      )}
    </li>
  );
});

/* rows a person opened out on a level, held by the history entry that
   shows it: a level opened afresh starts closed, and going back to one
   finds it as it was left. Nothing outlives the app. */
const openedByEntry = new WeakMap<object, ReadonlySet<string>>();
const NONE_OPENED: ReadonlySet<string> = new Set();

function useOpened(entry: object | undefined) {
  const [own] = useState(() => ({}));
  const holder = entry ?? own;
  const [state, setState] = useState(() => ({
    holder,
    keys: openedByEntry.get(holder) ?? NONE_OPENED,
  }));
  if (state.holder !== holder) {
    setState({ holder, keys: openedByEntry.get(holder) ?? NONE_OPENED });
  }
  const toggle = useCallback(
    (key: string, under: readonly string[]) =>
      setState((was) => {
        const keys = new Set(was.keys);
        if (!keys.delete(key)) keys.add(key);
        else for (const opened of under) keys.delete(opened);
        openedByEntry.set(was.holder, keys);
        return { ...was, keys };
      }),
    [],
  );
  return { keys: state.keys, toggle };
}

/* the rows opened inside the group of `rows[at]`: those after it that sit
   deeper, up to the next one that doesn't */
function openedUnder(rows: readonly Row[], at: number): string[] {
  const depth = rows[at]!.depth;
  const keys: string[] = [];
  for (let i = at + 1; i < rows.length && rows[i]!.depth > depth; i++) {
    if (rows[i]!.open) keys.push(sessionKeyOf(rows[i]!.session));
  }
  return keys;
}
