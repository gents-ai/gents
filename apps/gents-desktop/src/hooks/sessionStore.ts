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

export type SessionFacts = {
  rowsRevision: number;
  toolsRevision: number;
  tools: readonly RenderedToolCallView[];
  userRequestIds: ReadonlySet<string>;
};

export type SessionState = {
  session: DesktopSessionSnapshot | null;
  facts: SessionFacts;
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
  nodeDid: null,
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

export function heldFor<S extends { sessionId: string; nodeDid?: string | null }>(
  held: S | null,
  sessionId: string | null,
  nodeDid: string | null,
): S | null {
  return held?.sessionId === sessionId &&
    (!nodeDid || !held.nodeDid || held.nodeDid === nodeDid)
    ? held
    : null;
}

export function readSession(store: SessionStore): DesktopSessionSnapshot | null {
  return store.getState().session;
}

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
      item.kind === "pendingUserTurn"
        ? [item.requestId]
        : item.kind === "userMessage" || item.kind === "automatedInput"
          ? [
              item.inputRequestId ??
                (item.kind === "userMessage" && item.ownsTurn ? item.requestId : null),
            ].filter((id): id is string => Boolean(id))
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
    (session.pendingQueue ?? []).some((entry) => entry.requestId === sent.requestId) ||
    session.foldedInputs.some((folded) => folded.requestId === sent.requestId) ||
    (latest !== null && latest !== sent.latestWhenSent)
  );
}

export type SessionHeader = Pick<
  DesktopSessionSnapshot,
  | "sessionId"
  | "nodeDid"
  | "agentId"
  | "turnState"
  | "inferenceActivity"
  | "latestRequestId"
  | "pendingTurn"
  | "queuedTurns"
  | "pendingQueue"
  | "foldedInputs"
  | "hydration"
>;

export function headerOf(session: DesktopSessionSnapshot | null): SessionHeader | null {
  if (!session) return null;
  return {
    sessionId: session.sessionId,
    nodeDid: session.nodeDid,
    agentId: session.agentId,
    turnState: session.turnState,
    inferenceActivity: session.inferenceActivity,
    latestRequestId: session.latestRequestId,
    pendingTurn: session.pendingTurn,
    queuedTurns: session.queuedTurns,
    pendingQueue: session.pendingQueue,
    foldedInputs: session.foldedInputs,
    hydration: session.hydration,
  };
}

export function useSessionValue<T>(
  store: SessionStore,
  select: (state: SessionState) => T,
): T {
  return useStore(store, select);
}

export function useSessionFields<T extends object | null>(
  store: SessionStore,
  select: (state: SessionState) => T,
): T {
  return useStore(store, useShallow(select));
}
