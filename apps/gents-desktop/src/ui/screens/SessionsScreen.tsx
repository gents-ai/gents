/* Sessions: a heading row with search and New, then plain rows on the
   ground: title, the behavior's chip, and when it last moved. */
import { memo, useEffect, useMemo, useState } from "react";
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
import { Age } from "./time";
import {
  defaultScope,
  knownNodeIds,
  nodesInScope,
  sessionsInScope,
  type Scope,
} from "@/lib/scope";
import { useHomeDid, useScopeContext } from "@/hooks/useClient";
import { nodeDidOf, nodeOfSession } from "@/lib/nodes";
import { useStoredStrings } from "@/lib/stored";
import { NodeBehaviorStack } from "./NodeBehaviorStack";
import { NodeAxis } from "./NodeAxis";
import { SessionStatus } from "./SessionStatus";
import { isLive } from "@/lib/live";
import {
  SessionFilters,
  filterSessions,
  hasFilter,
  useSessionFilter,
} from "./SessionFilters";
import {
  NO_SESSIONS,
  parentOf as parentOfIn,
  useFleet,
  workersOf,
} from "../hooks/useFleet";
import type { NodeView } from "../../hooks/fleetStore";
import {
  useDeployments,
  useSelectedAgentDid,
  useSelectedDeployment,
} from "@/hooks/useClient";

export function SessionsScreen({
  nodeDid,
}: {
  /** a node named on the route: the list opens narrowed to it */
  nodeDid?: string;
}) {
  const deployments = useDeployments();
  const homeDid = useHomeDid();
  const selectedAgentDid = useSelectedAgentDid();
  const deployment = useSelectedDeployment();
  const [query, setQuery] = useState<string | null>(null);
  /* the three axes the summary carries: behavior, state, and what started it */
  const [filter, setFilter] = useSessionFilter();
  /* which parents are showing their workers; a person who opened one meant it */
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  /* the nodes the list shows: the working node to start, then whatever the
     chips choose; none chosen means every node */
  const fleet = useFleet((s) => s);
  const ctx = useScopeContext();
  /* the nodes as rows draw them, without their lists: the same objects
     while unchanged, so a row whose session did not change skips */
  const nodeList = useMemo(
    () => fleet.nodeKeys.map((key) => fleet.nodes[key]!),
    [fleet.nodeKeys, fleet.nodes],
  );
  const selectedNode = (selectedAgentDid && fleet.nodes[selectedAgentDid]) || null;
  const defaultNodeIds = nodesInScope(defaultScope("sessions"), ctx).map(nodeDidOf);
  const [storedNodeIds, setNodeIds] = useStoredStrings(
    "gents-prototype-sessions-nodes",
    defaultNodeIds,
  );
  useEffect(() => {
    if (nodeDid) setNodeIds([nodeDid]);
  }, [nodeDid, setNodeIds]);
  /* a pick whose nodes are all gone starts over at the working node */
  const knownIds = knownNodeIds(storedNodeIds, ctx);
  const nodeIds =
    storedNodeIds.length > 0 && knownIds.length === 0 ? defaultNodeIds : knownIds;
  /* the working node is where the list starts, not a filter to clear */
  const nodesPicked =
    nodeIds.length !== defaultNodeIds.length ||
    nodeIds.some((id) => !defaultNodeIds.includes(id));
  const scope: Scope = { nodes: nodeIds.length ? nodeIds : "all", agents: [] };
  const inScope = sessionsInScope(scope, ctx);
  const nodeCounts = Object.fromEntries(
    fleet.nodeKeys.map((key) => [key, fleet.sessionsOf[key]?.length ?? 0]),
  );
  const conversations = filterSessions(inScope, filter, query);
  /* an empty list says "yet" only when no node has a session; otherwise
     something narrowed it, the node chips included */
  const narrowed =
    Boolean(query) ||
    hasFilter(filter) ||
    fleet.nodeKeys.some((key) => (fleet.sessionsOf[key]?.length ?? 0) > 0);
  /* the session whose latest request spawned this one, by provenance */
  const parentOf = (c: SessionSummary) => parentOfIn(fleet, c);

  /* Work a session handed out sits under the session that handed it out.
     A parent with four workers is one piece of work in five sessions, and
     a flat list sorted by recency interleaves them with everything else
     the moment anything older moves.

     A child follows its parent only when the parent is here to follow: a
     filter or a search that keeps the child and drops the parent leaves
     the child where it fell, with the mark it already wears saying it came
     from somewhere. Nothing is hidden to make the shape tidy. */
  /* Work handed out is folded under the work that handed it out: a parent
     with four workers is one piece of work, and a list that spends five
     rows on it stops being a list of what a person is doing.

     Folded is not hidden. A worker that failed or is running is on the
     list whether or not its parent is open — the status
     mark is the reason to read this screen, and a fold that costs someone
     that is worse than the rows it saved. So is a worker that matches a
     filter: a person who asked for what needs them has asked for exactly
     these. */
  type Row =
    | {
        kind: "session";
        session: SessionSummary;
        child: boolean;
        delay?: number | null;
      }
    | {
        kind: "toggle";
        of: string;
        kin: number;
        hidden: number;
        open: boolean;
        running: number;
      };

  /* Work handed out sits under the work that handed it out — but only when
     someone asks to see it.

     Showing a worker the moment it became busy and folding it away when it
     settled made the list move on its own: rows appearing under the cursor,
     rows leaving from under it, everything below shifting each time. A list
     a person is reading should not rearrange itself because a machine got
     on with something. Opening and closing is theirs to decide, and what
     they decided holds.

     Nothing is lost by it. The parent says what its workers are doing and
     how many need someone, so the reason to look is on the screen; the
     looking is a click. */
  /* every worker on any node by the session that handed it out, whether
     or not the list is showing it: the parent's marks say the whole piece
     of work, and a worker on another node is still its work */
  const nested = (() => {
    const shown = new Set(conversations.map((c) => c.sessionId));
    const children = new Map<string, SessionSummary[]>();
    const roots: SessionSummary[] = [];
    for (const c of conversations) {
      const parent = parentOf(c);
      if (parent && shown.has(parent.sessionId)) {
        const kin = children.get(parent.sessionId) ?? [];
        kin.push(c);
        children.set(parent.sessionId, kin);
      } else roots.push(c);
    }
    return roots.flatMap((root) => {
      const kin = children.get(root.sessionId) ?? [];
      /* a filter is the person asking for exactly these, so it opens them */
      const open = expanded.has(root.sessionId) || hasFilter(filter);
      const rows: Row[] = [{ kind: "session", session: root, child: false }];
      if (open)
        for (const [n, c] of kin.entries())
          rows.push({ kind: "session", session: c, child: true, delay: n });
      const running = kin.filter((c) => isLive(c.turnState)).length;
      /* the way in stays while there is something behind it worth opening,
         or while it is open; a parent whose workers have all finished is
         one row again */
      if (kin.length > 0 && (open || running > 0))
        rows.push({
          kind: "toggle",
          of: root.sessionId,
          kin: kin.length,
          hidden: open ? 0 : kin.length,
          open,
          running,
        });
      return rows;
    });
  })();

  return (
    <ScrollArea className="h-full" data-testid="sessions-screen">
      <div className="mx-auto max-w-page px-6 py-6">
        <div className="flex h-10 items-center gap-2">
          <h1 className="font-heading text-lg font-medium text-heading">Sessions</h1>
          <div className="ml-auto flex min-w-0 items-center gap-2">
            <NodeAxis
              nodes={deployments}
              homeDid={homeDid}
              counts={nodeCounts}
              value={nodeIds}
              onChange={(next) => {
                setNodeIds(next);
                /* a pick is the person's, so the route stops naming a node */
                if (nodeDid) navigate({ name: "sessions" });
              }}
            />
            <SessionFilters
              sessions={inScope}
              deployment={deployment}
              value={filter}
              onChange={setFilter}
              nodes={{
                picked: nodesPicked,
                clear: () => setNodeIds(defaultNodeIds),
              }}
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
        <ul className="mt-4 divide-y divide-border/60">
          {nested.map((row) =>
            row.kind === "toggle" ? (
              <li key={`${row.of}-workers`}>
                <button
                  type="button"
                  aria-expanded={row.open}
                  className="-mx-3 flex w-[calc(100%+1.5rem)] cursor-pointer items-center gap-1.5 rounded-lg py-2.5 pl-12 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
                  onClick={() =>
                    setExpanded((was) => {
                      const next = new Set(was);
                      if (!next.delete(row.of)) next.add(row.of);
                      return next;
                    })
                  }
                >
                  <ChevronDown
                    className={cn(
                      "size-3.5 transition-transform",
                      row.open && "rotate-180",
                    )}
                  />
                  {row.open || row.hidden === 0
                    ? `${row.kin} ${row.kin === 1 ? "worker" : "workers"}`
                    : `${row.hidden} more ${row.hidden === 1 ? "worker" : "workers"}`}
                  {row.running > 0 && <span>· {row.running} running</span>}
                </button>
              </li>
            ) : (
              <SessionRow
                key={row.session.sessionId}
                session={row.session}
                child={row.child}
                delay={row.delay ?? null}
                parent={parentOf(row.session)}
                nodes={nodeList}
                homeDid={homeDid}
                deployment={selectedNode}
                workers={row.child ? NO_SESSIONS : workersOf(fleet, row.session)}
              />
            ),
          )}
          {conversations.length === 0 && narrowed && (
            <li className="py-8 text-center text-sm text-muted-foreground">
              No sessions match.
            </li>
          )}
          {conversations.length === 0 && !narrowed && (
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
      </div>
    </ScrollArea>
  );
}

/* One row of the list. A fleet read that changed one session re-renders that
   session's row: the others skip, since every prop keeps its identity while
   unchanged (the fleet store reconciles each read by key). */
const SessionRow = memo(function SessionRow({
  session,
  child,
  delay,
  parent,
  nodes,
  homeDid,
  deployment,
  workers,
}: {
  session: SessionSummary;
  child: boolean;
  delay: number | null;
  parent: SessionSummary | null;
  nodes: readonly NodeView[];
  homeDid: string | null | undefined;
  deployment: NodeView | null;
  workers: readonly SessionSummary[];
}) {
  return (
    <li
      key={session.sessionId}
      className={cn(
        delay != null &&
          "animate-in fade-in-0 slide-in-from-top-1 fill-mode-both duration-200 ease-out motion-reduce:animate-none",
      )}
      style={delay != null ? { animationDelay: `${delay * 40}ms` } : undefined}
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
        )}
      >
        <SessionStatus turnState={session.turnState} />
        <p className="flex min-w-0 items-center gap-1.5 text-sm">
          {/* the mark says a session came from another; the indent
              says which one. Kept in both places: a row that only
              moves right reads as a wrapped title, and a child
              whose parent is filtered away has nothing but this */}
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
        <NodeBehaviorStack
          nodes={nodes}
          homeDid={homeDid}
          nodeDid={nodeOfSession(session)}
          behaviorId={session.behaviorId}
          deployment={deployment}
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
