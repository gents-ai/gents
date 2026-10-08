import type { SetStateAction } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";
import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import type { SessionLoadState } from "../lib/loadingStatus";

/**
 * What the screens ask of a session's transcript besides drawing it, kept
 * when the session is written rather than worked out on every render. The
 * paging merge keeps an unchanged item as the same object, so a change is
 * found by identity; a streamed chunk replaces only the live reply, which
 * none of these count.
 */
export type SessionFacts = {
  /** advances when a row other than the live reply is added, removed or changed */
  rowsRevision: number;
  /** advances when a tool group is added, removed or changed */
  toolsRevision: number;
  /** every tool call, in transcript order; the same array until a tool changes */
  tools: readonly RenderedToolCallView[];
  /** the request ids on the transcript's input rows: a pending turn's
      request id, and a saved or automated entry's input request id
      (`inputRequestId`), falling back to its document id */
  userRequestIds: ReadonlySet<string>;
};

export type SessionState = {
  /** the selected session as last read; never persisted */
  session: DesktopSessionSnapshot | null;
  facts: SessionFacts;
  /** the last read asked for, and how it went */
  load: SessionLoadState;
};

export type SessionStore = StoreApi<SessionState>;

const NO_ITEMS: readonly RenderedTimelineItem[] = [];
const NO_TOOLS: readonly RenderedToolCallView[] = [];
const NO_REQUESTS: ReadonlySet<string> = new Set();
const NO_FACTS: SessionFacts = {
  rowsRevision: 0,
  toolsRevision: 0,
  tools: NO_TOOLS,
  userRequestIds: NO_REQUESTS,
};

export const IDLE_LOAD: SessionLoadState = {
  phase: "idle",
  sessionId: null,
  agentDid: null,
  found: null,
  error: null,
};

export function createSessionStore(session: DesktopSessionSnapshot | null = null) {
  const store = createStore<SessionState>(() => ({
    session: null,
    facts: NO_FACTS,
    load: IDLE_LOAD,
  }));
  if (session) writeSession(store, session);
  return store;
}

/** The held read when it is the selected session's: a read for the previous
    selection can still be held while the next one loads. */
export function heldFor<S extends { sessionId: string; agentDid?: string | null }>(
  held: S | null,
  sessionId: string | null,
  agentDid: string | null,
): S | null {
  return held?.sessionId === sessionId &&
    (!agentDid || !held.agentDid || held.agentDid === agentDid)
    ? held
    : null;
}

export function readSession(store: SessionStore): DesktopSessionSnapshot | null {
  return store.getState().session;
}

/** Commits a read. The same snapshot again notifies no one. */
export function writeSession(
  store: SessionStore,
  next: SetStateAction<DesktopSessionSnapshot | null>,
) {
  store.setState((state) => {
    const session = typeof next === "function" ? next(state.session) : next;
    if (session === state.session) return state;
    return { ...state, session, facts: factsOf(state, session) };
  });
}

export function writeSessionLoad(store: SessionStore, load: SessionLoadState) {
  store.setState({ load });
}

/* the same items in the same order, the live reply left out */
function sameItems(
  before: readonly RenderedTimelineItem[],
  after: readonly RenderedTimelineItem[],
  counts: (item: RenderedTimelineItem) => boolean,
) {
  let i = 0;
  let j = 0;
  for (;;) {
    while (i < before.length && !counts(before[i]!)) i += 1;
    while (j < after.length && !counts(after[j]!)) j += 1;
    if (i === before.length || j === after.length)
      return i === before.length && j === after.length;
    if (before[i] !== after[j]) return false;
    i += 1;
    j += 1;
  }
}

const isRow = (item: RenderedTimelineItem) => item.kind !== "liveAssistant";
const isToolGroup = (item: RenderedTimelineItem) => item.kind === "toolGroup";

function factsOf(
  state: SessionState,
  next: DesktopSessionSnapshot | null,
): SessionFacts {
  const before = state.session?.timelineItems ?? NO_ITEMS;
  const after = next?.timelineItems ?? NO_ITEMS;
  const facts = state.facts;
  const rowsSame = sameItems(before, after, isRow);
  const toolsSame = rowsSame || sameItems(before, after, isToolGroup);
  if (rowsSame) return facts;
  const userRequestIds = new Set(
    after.flatMap((item) =>
      item.kind === "userMessage" || item.kind === "automatedInput"
        ? [item.inputRequestId, item.requestId].filter((id): id is string =>
            Boolean(id),
          )
        : item.kind === "pendingUserTurn" && item.requestId
          ? [item.requestId]
          : [],
    ),
  );
  return {
    rowsRevision: facts.rowsRevision + 1,
    toolsRevision: toolsSame ? facts.toolsRevision : facts.toolsRevision + 1,
    tools: toolsSame
      ? facts.tools
      : after.flatMap((item) => (item.kind === "toolGroup" ? item.tools : [])),
    userRequestIds,
  };
}

/**
 * Whether the bridge holds a request the app sent, by the request's id (what
 * a send returns): once it does, the transcript has a row for it (its pending
 * turn, or its saved or folded entry by `inputRequestId`), the session lists
 * it as queued behind the running turn or as folded into a claimed turn, or
 * it is the session's latest request; and should no read happen to show any
 * of these, the latest has moved on from `latestWhenSent`, the one the
 * session showed when the request was sent.
 */
export function holdsRequest(
  state: SessionState,
  sent: { sessionId: string; requestId: string; latestWhenSent: string | null },
): boolean {
  const session = state.session;
  if (session?.sessionId !== sent.sessionId) return false;
  const latest = session.latestRequestId;
  return (
    latest === sent.requestId ||
    state.facts.userRequestIds.has(sent.requestId) ||
    session.queuedTurns.some((turn) => turn.requestId === sent.requestId) ||
    session.foldedInputs.some((folded) => folded.requestId === sent.requestId) ||
    (latest !== null && latest !== sent.latestWhenSent)
  );
}

/** The session fields the projection decides with. A streamed chunk changes
    the transcript and the revision but none of these, so the projection
    keeps its previous header and what reads it does not re-render. */
export type SessionHeader = Pick<
  DesktopSessionSnapshot,
  | "sessionId"
  | "agentDid"
  | "behaviorId"
  | "turnState"
  | "latestRequestId"
  | "pendingTurn"
  | "queuedTurns"
  | "foldedInputs"
  | "hydration"
>;

export function headerOf(session: DesktopSessionSnapshot | null): SessionHeader | null {
  if (!session) return null;
  return {
    sessionId: session.sessionId,
    agentDid: session.agentDid,
    behaviorId: session.behaviorId,
    turnState: session.turnState,
    latestRequestId: session.latestRequestId,
    pendingTurn: session.pendingTurn,
    queuedTurns: session.queuedTurns,
    foldedInputs: session.foldedInputs,
    hydration: session.hydration,
  };
}

/** A value derived from the session store; re-renders when it changes by identity. */
export function useSessionValue<T>(
  store: SessionStore,
  select: (state: SessionState) => T,
): T {
  return useStore(store, select);
}

/** An object of fields; re-renders when any of them changes. */
export function useSessionFields<T extends object | null>(
  store: SessionStore,
  select: (state: SessionState) => T,
): T {
  return useStore(store, useShallow(select));
}
