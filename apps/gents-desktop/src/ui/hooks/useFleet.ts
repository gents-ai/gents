import { useStore } from "zustand";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { nodeKeyOf, sessionKeyOf, type FleetState } from "../../hooks/fleetStore";
import { useApp } from "../app/AppContext";
import { isLive, stateOf, turnLabel } from "../lib/live";

export const NO_SESSIONS: readonly SessionSummary[] = [];

export function useFleet<T>(select: (state: FleetState) => T): T {
  return useStore(useApp().stores.fleet, select);
}

export const parentOf = (state: FleetState, session: SessionSummary) =>
  state.parentOf[sessionKeyOf(session)] ?? null;

export const workersOf = (state: FleetState, session: SessionSummary) =>
  state.workersOf[sessionKeyOf(session)] ?? NO_SESSIONS;

export function workersOfId(state: FleetState, sessionId: string | null | undefined) {
  const session = sessionId ? state.bySessionId[sessionId] : undefined;
  return session ? workersOf(state, session) : NO_SESSIONS;
}

/* how many of the sessions `session` started hold, themselves or through
   a session under them */
function countUnder(
  state: FleetState,
  session: SessionSummary,
  holds: (s: SessionSummary) => boolean,
): number {
  const seen = new Set<string>();
  const under = (s: SessionSummary): boolean => {
    const key = sessionKeyOf(s);
    if (seen.has(key)) return false;
    seen.add(key);
    return holds(s) || workersOf(state, s).some(under);
  };
  seen.add(sessionKeyOf(session));
  return workersOf(state, session).filter(under).length;
}

/** Of the sessions `session` started, the ones working, or with work running
    anywhere under them. */
export const runningUnder = (state: FleetState, session: SessionSummary) =>
  countUnder(state, session, (s) => isLive(s.turnState));

export const failedUnder = (state: FleetState, session: SessionSummary) =>
  countUnder(state, session, (s) => stateOf(s.turnState) === "failed");

/* stopped by someone, not replaced by an edit */
export const stoppedUnder = (state: FleetState, session: SessionSummary) =>
  countUnder(state, session, (s) => turnLabel(s.turnState) === "Stopped");

/** What a node has going on: its running sessions and its open mailbox
    items, as the Nodes page, the node switcher and the nav count them. */
export function activityOf(
  state: Pick<FleetState, "sessionsOf" | "mailboxOf">,
  nodeDid: string,
) {
  const key = nodeKeyOf({ nodeDid });
  return {
    running: (state.sessionsOf[key] ?? NO_SESSIONS).filter((c) => isLive(c.turnState))
      .length,
    waiting: (state.mailboxOf[key] ?? []).filter((m) => m.status === "open").length,
  };
}

/** The same, in words: "2 running · 1 waiting", or nothing when idle. */
export const activityText = ({
  running,
  waiting,
}: {
  running: number;
  waiting: number;
}) =>
  [running && `${running} running`, waiting && `${waiting} waiting`]
    .filter(Boolean)
    .join(" · ");
