import { useEffect } from "react";

import { useApp } from "../app/AppContext";
import type { Route } from "../lib/router";
import { useFleet } from "./useFleet";

/**
 * The selection the route asks for: the session it names, or the
 * new-session screen. Asked again when the nodes' session lists change,
 * since a session on another node can only be followed once that node
 * lists it.
 */
export function useFollowRoute(route: Route) {
  const { followRoute } = useApp().actions;
  const sessionId = route.name === "session" ? route.sessionId : undefined;
  const listed = useFleet((state) => state.sessionsOf);
  useEffect(() => {
    if (sessionId !== undefined) followRoute(sessionId);
  }, [followRoute, sessionId]);
  useEffect(() => {
    if (sessionId) followRoute(sessionId, true);
  }, [followRoute, sessionId, listed]);
}
