import { useStore } from "zustand";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { nodeKeyOf, sessionKeyOf, type FleetState } from "../../hooks/fleetStore";
import { useApp } from "../app/AppContext";
import { isLive } from "../lib/live";
import { liftedStates, type StartedCounts } from "../lib/session-rows";

export const NO_SESSIONS: readonly SessionSummary[] = [];
const NO_KEYS: ReadonlySet<string> = new Set();

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

/** How many sessions `session` started, and of those the ones running,
    failed or stopped, themselves or anywhere under them. Sessions in
    `above`, and `session` itself, are left out, so a loop in who started
    whom ends where it began. */
export function startedUnder(
  state: FleetState,
  session: SessionSummary,
  above: ReadonlySet<string> = NO_KEYS,
): StartedCounts {
  const own = sessionKeyOf(session);
  const outside = (s: SessionSummary) => {
    const key = sessionKeyOf(s);
    return key !== own && !above.has(key);
  };
  return liftedStates((s) => workersOf(state, s).filter(outside)).counts(session);
}

const NONE_STARTED: StartedCounts = { started: 0, running: 0, failed: 0, stopped: 0 };

export function startedUnderId(
  state: FleetState,
  sessionId: string | null | undefined,
) {
  const session = sessionId ? state.bySessionId[sessionId] : undefined;
  return session ? startedUnder(state, session) : NONE_STARTED;
}

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
