/* A session's display rows held beyond the session snapshot's newest page.
   The snapshot owns the newest stretch: its rows, the pending turns placed
   among them and the live reply. History before it is read by message
   number and held here as one continuous stretch; a jump replaces the
   stretch, which may then sit apart from the newest page until the reads
   between them close the gap. */
import type {
  RenderedTimelineItem,
  SessionRowsView,
  SessionRowsWindow,
} from "@source-inc/gents-desktop-client";
import { removeMaterializedPendingTurns } from "@source-inc/gents-desktop-client";

/* the message number a drawn row belongs to; a tool group rides with the
   message that called it; pending turns and the live reply have none */
export function sequenceOf(item: RenderedTimelineItem): number | null {
  switch (item.kind) {
    case "toolGroup":
      return item.messageSequence ?? null;
    case "userMessage":
    case "assistantMessage":
    case "automatedInput":
    case "sessionCompaction":
    case "requestContextReductionSummary":
      return item.sequence ?? null;
    default:
      return null;
  }
}

const identity = (item: RenderedTimelineItem) => `${item.kind}:${item.itemKey}`;
/* at one message number: the message, the tool group that rides with it,
   then the context events anchored to it, as the bridge places them */
const RANK: Partial<Record<RenderedTimelineItem["kind"], number>> = {
  toolGroup: 1,
  sessionCompaction: 2,
  requestContextReductionSummary: 3,
};
const rank = (item: RenderedTimelineItem) => RANK[item.kind] ?? 0;

export type HeldRows = {
  sessionId: string | null;
  rows: RenderedTimelineItem[];
  /* the message numbers read, inclusive; null before any read */
  from: number | null;
  through: number | null;
  /* the read reached the newest message the session had then */
  reachesNewest: boolean;
  hasOlder: boolean;
  /* every read came from a source that had the whole session */
  complete: boolean;
};

export const noRows = (sessionId: string | null): HeldRows => ({
  sessionId,
  rows: [],
  from: null,
  through: null,
  reachesNewest: true,
  hasOlder: true,
  complete: true,
});

function ordered(rows: RenderedTimelineItem[]): RenderedTimelineItem[] {
  const byIdentity = new Map<string, RenderedTimelineItem>();
  for (const row of rows) {
    if (sequenceOf(row) !== null) byIdentity.set(identity(row), row);
  }
  return [...byIdentity.values()].sort(
    (a, b) => sequenceOf(a)! - sequenceOf(b)! || rank(a) - rank(b),
  );
}

/* a read's rows join what is held: a jump replaces it, a read before or
   after extends it, and a newer copy of a row replaces the older one */
export function applyRowsRead(
  held: HeldRows,
  window: SessionRowsWindow,
  read: SessionRowsView,
): HeldRows {
  if (read.sessionId !== held.sessionId) return held;
  const complete = read.complete && (window.kind === "around" || held.complete);
  switch (window.kind) {
    case "tip":
    case "around":
      return {
        sessionId: held.sessionId,
        rows: ordered(read.rows),
        from: read.coveredFrom,
        through: read.coveredThrough,
        reachesNewest: read.coveredThrough === null,
        hasOlder: read.hasOlder,
        complete,
      };
    case "before":
      return {
        ...held,
        rows: ordered([...read.rows, ...held.rows]),
        from: read.coveredFrom,
        through: held.through ?? read.coveredThrough,
        hasOlder: read.hasOlder,
        complete,
      };
    case "after":
      return {
        ...held,
        rows: ordered([...held.rows, ...read.rows]),
        from: held.from ?? read.coveredFrom,
        through: read.coveredThrough,
        reachesNewest: read.coveredThrough === null,
        complete,
      };
  }
}

export type DrawnRows = {
  items: RenderedTimelineItem[];
  /* the newest page is drawn after the held rows */
  joined: boolean;
  /* where the next older read ends, or null when nothing is older */
  olderBefore: number | null;
  /* where the next newer read starts while apart, else null */
  newerAfter: number | null;
};

/* the rows to draw: held history before the snapshot's first saved row,
   then the snapshot's own items in its order; held rows apart from the
   newest page are drawn alone until the gap is read */
export function drawnRows(
  held: HeldRows,
  newest: RenderedTimelineItem[],
  newestHasOlder: boolean,
): DrawnRows {
  let first: number | null = null;
  for (const item of newest) {
    const at = sequenceOf(item);
    if (at !== null && (first === null || at < first)) first = at;
  }
  const joined =
    held.reachesNewest ||
    held.through === null ||
    first === null ||
    held.through >= first - 1;
  if (!joined) {
    return {
      items: held.rows,
      joined: false,
      olderBefore: held.hasOlder && held.from !== null ? held.from : null,
      newerAfter: held.through,
    };
  }
  const before = held.rows.filter((row) => first === null || sequenceOf(row)! < first);
  const top =
    held.from !== null && (first === null || held.from < first) ? held.from : first;
  const hasOlder = held.from !== null ? held.hasOlder : newestHasOlder;
  return {
    /* a pending turn whose saved row came in with older history is dropped */
    items: removeMaterializedPendingTurns([...before, ...newest]),
    joined: true,
    olderBefore: hasOlder && top !== null && top > 0 ? top : null,
    newerAfter: null,
  };
}
