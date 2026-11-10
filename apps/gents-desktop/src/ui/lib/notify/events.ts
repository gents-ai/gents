import {
  parseMailboxQuestion,
  type MailboxItemView,
  type SessionSummary,
} from "@source-inc/gents-desktop-client";
import type { FleetState } from "../../../hooks/fleetStore";
import { sessionKeyOf } from "../../../hooks/fleetStore";
import type { Route } from "../router";
import { stateOf } from "../live";

/** The kinds of thing the app tells the person about, each switched on or
    off on this device. */
export type NotifyKind = "finished" | "failed" | "question" | "mailbox";

/** Something that happened, as one line of a notification. `key` is unique
    per occurrence, so the same request or item is never told twice. */
export type NotifyEvent = {
  kind: NotifyKind;
  key: string;
  /** a session's or a mailbox item's title; never transcript content */
  title: string;
  /** the session it is about, to stay quiet while it is on screen */
  session: { nodeDid: string; sessionId: string } | null;
  /** a worker, a session another session started */
  worker: boolean;
  /** where clicking it leads */
  to: Route;
};

const titled = (session: SessionSummary) => session.title ?? "Untitled session";

const sessionRoute = (s: { nodeDid: string; sessionId: string }): Route => ({
  name: "session",
  sessionId: s.sessionId,
  nodeDid: s.nodeDid,
});

const ended = (session: SessionSummary) => {
  const state = stateOf(session.turnState);
  return state === "finished" || state === "failed";
};

/** What changed between two views of the fleet that the person may want
    to hear about: a session's latest request that finished or failed, and
    a mailbox item or question that arrived. Nothing from before `since`
    counts: a session first seen in `next` was not watched while it ran,
    and an item created before it is history a node is still syncing. */
export function changesBetween(
  prev: Pick<FleetState, "sessions" | "mailboxOf">,
  next: Pick<FleetState, "sessions" | "mailboxOf">,
  since: string,
): NotifyEvent[] {
  const events: NotifyEvent[] = [];
  for (const [key, session] of Object.entries(next.sessions)) {
    if (!session.latestRequestId || !ended(session)) continue;
    const before = prev.sessions[key];
    if (!before) continue;
    if (before.latestRequestId === session.latestRequestId && ended(before)) continue;
    const failed = stateOf(session.turnState) === "failed";
    const worker = session.startedBy !== null;
    /* a worker that finished is its starter's business; one that failed
       may need the person */
    if (worker && !failed) continue;
    events.push({
      kind: failed ? "failed" : "finished",
      key: `${sessionKeyOf(session)}\u0000${session.latestRequestId}`,
      title: titled(session),
      session: { nodeDid: session.nodeDid, sessionId: session.sessionId },
      worker,
      to: sessionRoute(session),
    });
  }
  const known = new Set(
    Object.values(prev.mailboxOf)
      .flat()
      .map((m) => m.itemId),
  );
  const after = Date.parse(since);
  for (const item of Object.values(next.mailboxOf).flat()) {
    if (item.status !== "open" || known.has(item.itemId)) continue;
    if (!(Date.parse(item.createdAt) > after)) continue;
    events.push(itemEvent(item, next));
  }
  return events;
}

function itemEvent(
  item: MailboxItemView,
  fleet: Pick<FleetState, "sessions">,
): NotifyEvent {
  const question = item.kind === "ask" && parseMailboxQuestion(item) !== null;
  const session =
    item.sessionId !== null
      ? { nodeDid: item.targetNodeDid ?? item.nodeDid, sessionId: item.sessionId }
      : null;
  const owner = session
    ? Object.values(fleet.sessions).find(
        (s) => s.sessionId === session.sessionId && s.nodeDid === session.nodeDid,
      )
    : undefined;
  return {
    kind: question ? "question" : "mailbox",
    key: `item\u0000${item.itemId}`,
    title: question && owner ? titled(owner) : item.title,
    session,
    worker: false,
    to: question && session ? sessionRoute(session) : { name: "mailbox" },
  };
}
