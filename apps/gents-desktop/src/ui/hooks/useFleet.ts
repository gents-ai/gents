import { useStore } from "zustand";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { sessionKeyOf, type FleetState } from "../../hooks/fleetStore";
import { useApp } from "../app/AppContext";
import { isLive } from "../lib/live";

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

/** Of the sessions `session` started, the ones working, or with work running
    anywhere under them. */
export function runningUnder(state: FleetState, session: SessionSummary): number {
  const seen = new Set<string>();
  const working = (s: SessionSummary): boolean => {
    const key = sessionKeyOf(s);
    if (seen.has(key)) return false;
    seen.add(key);
    return isLive(s.turnState) || workersOf(state, s).some(working);
  };
  seen.add(sessionKeyOf(session));
  return workersOf(state, session).filter(working).length;
}
