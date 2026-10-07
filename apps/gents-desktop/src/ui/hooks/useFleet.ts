import { useStore } from "zustand";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

import { sessionKeyOf, type FleetState } from "../../hooks/fleetStore";
import { useApp } from "../app/AppContext";

export const NO_SESSIONS: readonly SessionSummary[] = [];

/** A value from the fleet store; re-renders when it changes by identity. */
export function useFleet<T>(select: (state: FleetState) => T): T {
  return useStore(useApp().stores.fleet, select);
}

/** The session that handed `session` out, on whatever node lists it. */
export const parentOf = (state: FleetState, session: SessionSummary) =>
  state.parentOf[sessionKeyOf(session)] ?? null;

/** The sessions `session` handed out, on any node. */
export const workersOf = (state: FleetState, session: SessionSummary) =>
  state.workersOf[sessionKeyOf(session)] ?? NO_SESSIONS;

/** The same, for a session known only by its id, such as a route's. */
export function workersOfId(state: FleetState, sessionId: string | null | undefined) {
  const session = sessionId ? state.bySessionId[sessionId] : undefined;
  return session ? workersOf(state, session) : NO_SESSIONS;
}
