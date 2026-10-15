/* Sessions: a heading row with search and New, then plain rows on the
   ground: title, the agent's chip, and when it last moved. */
import {
  memo,
  useCallback,
  useLayoutEffect,
  useMemo,
  useState,
  type ComponentProps,
} from "react";
import { useShallow } from "zustand/react/shallow";
import {
  ChevronDown,
  CornerDownRight,
  Lock,
  MessageSquare,
  Play,
  Plus,
  Search,
  X,
} from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { href, navigate } from "@/lib/router";
import { useListWindow } from "@/lib/listWindow";
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
import { SessionFilters, matchesView } from "./SessionFilters";
import { emptyFilter, hasFilter } from "@/lib/session-filter";
import { sessionRows, type SessionRow as Row } from "@/lib/session-rows";
import { listViews, useListViews } from "@/app/listViews";
import {
  NO_SESSIONS,
  parentOf as parentOfIn,
  useFleet,
  workersOf,
} from "../hooks/useFleet";

const rowKey = (row: Row) =>
  row.kind === "session" ? sessionKeyOf(row.session) : `${row.of}-workers`;
const rowHeight = (row: Row) => (row.kind === "toggle" ? 37 : row.child ? 41 : 61);

export function SessionsScreen({
  nodeDid,
}: {
  /** a node named on the route: the list opens narrowed to it */
  nodeDid?: string;
}) {
  const nodes = useFleet(useShallow(fleetNodes));
  const homeDid = useHomeDid();
  const selectedNodeDid = useSelectedNodeDid();
  const [query, setQuery] = useState<string | null>(null);
  /* the three axes the summary carries: agent, state, and what started it */
  const filter = useListViews((v) => v.sessionFilter);
  const openSessions = useListViews((v) => v.openSessions);
  /* the nodes the list shows: every node to start, then whatever the
     chips choose; none chosen means every node */
  const placement = { nodes, selectedNodeDid, homeDid };
  const defaultNodeIds = nodesInScope(defaultScope("sessions"), placement).map(
    nodeDidOf,
  );
  const storedNodeIds = useListViews((v) => v.sessionNodes);
  /* a node named on the route narrows the list to it, once per route,
     before the list is painted */
  useLayoutEffect(() => {
    if (nodeDid) listViews.setSessionNodes([nodeDid]);
  }, [nodeDid]);
  /* a pick whose nodes are all gone starts over at every node */
  const knownIds = storedNodeIds && knownNodeIds(storedNodeIds, placement);
  const nodeIds =
    !knownIds || (knownIds.length === 0 && storedNodeIds!.length > 0)
      ? defaultNodeIds
      : knownIds;
  /* the nodes the list spans narrow it only when they differ from where it
     starts; an empty pick is every node */
  const shownNodeIds = nodeIds.length ? nodeIds : nodes.map(nodeDidOf);
  const nodesPicked =
    shownNodeIds.length !== defaultNodeIds.length ||
    shownNodeIds.some((id) => !defaultNodeIds.includes(id));
  const scope: Scope = { nodes: nodeIds.length ? nodeIds : "all", agents: [] };
  const inScope = useInScope((ctx) => sessionsInScope(scope, ctx));
  const nodeCounts = useFleet(
    useShallow((s) =>
      Object.fromEntries(
        s.nodeKeys.map((key) => [key, s.sessionsOf[key]?.length ?? 0]),
      ),
    ),
  );
  /* the lineage of the sessions in scope only, so work elsewhere in the
     fleet does not re-render the list */
  const parents = useFleet(
    useShallow((s) =>
      Object.fromEntries(inScope.map((c) => [sessionKeyOf(c), parentOfIn(s, c)])),
    ),
  );
  const workers = useFleet(
    useShallow((s) =>
      Object.fromEntries(inScope.map((c) => [sessionKeyOf(c), workersOf(s, c)])),
    ),
  );
  /* nesting goes through starters the list leaves out (another node, a
     narrowed pick) to the nearest one it shows */
  const listedParents = useFleet(
    useShallow((s) => {
      const listed = new Set(inScope.map(sessionKeyOf));
      return Object.fromEntries(
        inScope.map((c) => {
          const seen = new Set([sessionKeyOf(c)]);
          let parent = parentOfIn(s, c);
          while (parent && !listed.has(sessionKeyOf(parent))) {
            if (seen.has(sessionKeyOf(parent))) return [sessionKeyOf(c), null];
            seen.add(sessionKeyOf(parent));
            parent = parentOfIn(s, parent);
          }
          return [sessionKeyOf(c), parent];
        }),
      );
    }),
  );
  const parentOf = useCallback(
    (c: SessionSummary) => parents[sessionKeyOf(c)] ?? null,
    [parents],
  );
  const filtering = hasFilter(filter) || Boolean(query);
  const { rows, shown } = useMemo(() => {
    const open = new Set(openSessions);
    return sessionRows({
      sessions: inScope,
      parentOf: (c) => listedParents[sessionKeyOf(c)] ?? null,
      matches: filtering ? (c) => matchesView(c, filter, query) : null,
      isOpen: (key) => open.has(key),
    });
  }, [inScope, listedParents, filter, query, filtering, openSessions]);
  /* an empty list says "yet" only when no node has a session; otherwise
     something narrowed it, the node chips included */
  const narrowed = filtering || Object.values(nodeCounts).some((count) => count > 0);
  const pickedNames = nodesPicked
    ? nodes
        .filter((n) => nodeIds.includes(nodeDidOf(n)))
        .map((n) => n.node.displayName ?? n.label)
    : [];

  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  const scrollRoot = useCallback((root: HTMLDivElement | null) => {
    setScroller(
      root?.querySelector<HTMLElement>('[data-slot="scroll-area-viewport"]') ?? null,
    );
  }, []);
  return (
    <ScrollArea ref={scrollRoot} className="h-full" data-testid="sessions-screen">
      <div className="mx-auto max-w-page px-6 py-6">
        <div className="flex h-10 items-center gap-2">
          <h1 className="font-heading text-lg font-medium text-heading">Sessions</h1>
          <div className="ml-auto flex min-w-0 items-center gap-2">
            <NodeAxis
              nodes={nodes}
              homeDid={homeDid}
              counts={nodeCounts}
              /* every node is the picker's open state, "All nodes", however
                 the list came to show them all */
              value={nodesPicked ? nodeIds : []}
              onChange={(next) => {
                listViews.setSessionNodes(next);
                /* a pick is the person's, so the route stops naming a node */
                if (nodeDid) navigate({ name: "sessions" });
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
            <Button
              variant="brand"
              size="sm"
              className="shrink-0"
              nativeButton={false}
              render={<a href={href({ name: "session", sessionId: null })} />}
            >
              {/* a phone row has no width for the word beside six marks */}
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
              placeholder="Search sessions"
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
                  ? `${shown} of ${inScope.length} session${inScope.length === 1 ? "" : "s"}`
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
                  if (nodeDid) navigate({ name: "sessions" });
                }}
              >
                Clear
              </Button>
            </div>
          )}
        </div>
        <SessionList
          rows={rows}
          empty={rows.length === 0}
          narrowed={narrowed}
          scroller={scroller}
          parentOf={parentOf}
          workers={workers}
          toggleOpen={listViews.toggleOpenSession}
        />
      </div>
    </ScrollArea>
  );
}

function SessionList({
  rows,
  empty,
  narrowed,
  scroller,
  parentOf,
  workers,
  toggleOpen,
}: {
  rows: Row[];
  empty: boolean;
  narrowed: boolean;
  scroller: HTMLElement | null;
  parentOf: (session: SessionSummary) => SessionSummary | null;
  workers: Record<string, readonly SessionSummary[]>;
  toggleOpen: (key: string) => void;
}) {
  const window = useListWindow(rows, scroller, rowKey, rowHeight);

  return (
    <ul
      {...window.listProps}
      aria-label="Sessions"
      data-session-count={rows.length}
      className={cn("mt-4", !window.enabled && "divide-y divide-border/60")}
    >
      {window.items.map(({ index, start }) => {
        const row = rows[index]!;
        const attributes = window.attributes(index, start);
        if (attributes && index > 0) attributes.className = "border-t border-border/60";
        return row.kind === "toggle" ? (
          <li key={rowKey(row)} {...attributes}>
            <button
              type="button"
              aria-expanded={row.open}
              className="-mx-3 flex w-[calc(100%+1.5rem)] cursor-pointer items-center gap-1.5 rounded-lg py-2.5 pl-12 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
              onClick={() => toggleOpen(row.of)}
            >
              <ChevronDown
                className={cn(
                  "size-3.5 transition-transform",
                  row.open && "rotate-180",
                )}
              />
              {`${row.kin} started session${row.kin === 1 ? "" : "s"}`}
              {row.running > 0 && <span>· {row.running} running</span>}
            </button>
          </li>
        ) : (
          <SessionRow
            key={rowKey(row)}
            attributes={attributes}
            session={row.session}
            child={row.child}
            context={row.context}
            delay={row.delay ?? null}
            parent={parentOf(row.session)}
            workers={
              row.child
                ? NO_SESSIONS
                : (workers[sessionKeyOf(row.session)] ?? NO_SESSIONS)
            }
          />
        );
      })}
      {empty && narrowed && (
        <li className="py-8 text-center text-sm text-muted-foreground">
          No sessions match.
        </li>
      )}
      {empty && !narrowed && (
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

/* One row of the list. A fleet read that changed one session re-renders that
   session's row: the others skip, since every prop keeps its identity while
   unchanged (the fleet store reconciles each read by key). */
const SessionRow = memo(function SessionRow({
  session,
  child,
  context,
  delay,
  parent,
  workers,
  attributes = {},
}: {
  attributes?: ComponentProps<"li">;
  session: SessionSummary;
  child: boolean;
  context: boolean;
  delay: number | null;
  parent: SessionSummary | null;
  workers: readonly SessionSummary[];
}) {
  return (
    <li
      {...attributes}
      className={cn(
        attributes.className,
        !attributes.style &&
          delay != null &&
          "animate-in fade-in-0 slide-in-from-top-1 fill-mode-both duration-200 ease-out motion-reduce:animate-none",
      )}
      style={
        attributes.style ??
        (delay != null ? { animationDelay: `${delay * 40}ms` } : undefined)
      }
    >
      <a
        href={href({
          name: "session",
          sessionId: session.sessionId,
        })}
        data-testid={`session-${session.sessionId}`}
        className={cn(
          "-mx-3 grid grid-cols-[auto_1fr_auto_auto] items-center gap-4 rounded-lg px-3 hover:bg-accent",
          /* under the work it came from, and smaller with it: a
             worker is a detail of the row above, and a list where
             everything is the same size has no shape to read */
          child ? "py-2 pl-9 text-sm" : "py-3.5",
          context && "opacity-60",
        )}
      >
        <SessionStatus turnState={session.turnState} />
        <p className="flex min-w-0 items-center gap-1.5 text-sm">
          {/* the mark says a session came from another; the indent
              says which one. Kept in both places: a row that only
              moves right reads as a wrapped title, and a session
              whose starter the list leaves out has nothing but this */}
          {/* a run nobody started by typing wears the mark of what
              did: the list filters on this and showed nothing */}
          {!parent && session.taskId && (
            <Play
              className="size-3.5 shrink-0 text-muted-foreground"
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
              className="size-3.5 shrink-0 text-muted-foreground"
              role="img"
              aria-label={`Started by ${parent?.title ?? "another session"}`}
            >
              <title>Started by {parent?.title ?? "another session"}</title>
            </CornerDownRight>
          )}
          {session.unreadableReason && (
            <Lock
              className="size-3.5 shrink-0 text-muted-foreground"
              role="img"
              aria-label={session.unreadableReason}
            >
              <title>{session.unreadableReason}</title>
            </Lock>
          )}
          <span
            className={cn(
              "truncate",
              (child || session.unreadableReason) && "text-muted-foreground",
            )}
          >
            {session.title ?? "Untitled"}
          </span>
        </p>
        <NodeAgentStack
          nodeDid={nodeOfSession(session)}
          agentId={session.agentId}
          size={child ? "sm" : "md"}
          workers={workers}
        />
        <span
          className={cn(
            "w-10 text-right text-muted-foreground sm:w-20",
            child ? "text-xs" : "text-sm",
          )}
        >
          {/* kept current by the clock, whether or not the row re-renders */}
          <Age iso={session.updatedAt} />
        </span>
      </a>
    </li>
  );
});
