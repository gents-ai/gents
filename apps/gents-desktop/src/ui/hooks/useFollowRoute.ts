import { useEffect } from "react";

import { useApp } from "../app/AppContext";
import { navigate, reviewNavigation, type Route } from "../lib/router";
import { nodeIsUnknown } from "../../hooks/fleetStore";
import { sessionDestination } from "../../hooks/selectionActions";
import { noteOpenSession } from "../../hooks/sessionTrail";
import { useFleet } from "./useFleet";

// Re-asked when the nodes' session lists change, since a session on another
// node can only be followed once that node lists it.
export function useFollowRoute(route: Route) {
  const {
    actions: { followRoute, selectNode },
    stores,
  } = useApp();
  const sessionId = route.name === "session" ? route.sessionId : undefined;
  const nodeDid = "nodeDid" in route ? route.nodeDid : undefined;
  const listed = useFleet((state) => state.sessionsOf);
  const unknownNode = useFleet((state) => nodeIsUnknown(state, nodeDid));
  function follow(fromSnapshot: boolean) {
    if (unknownNode) return;
    const previous = stores.selection.getState();
    const destination =
      nodeDid ??
      (sessionId
        ? sessionDestination(stores.fleet.getState(), previous, sessionId)
        : undefined);
    const target = destination ? ({ ...route, nodeDid: destination } as Route) : route;
    reviewNavigation(
      target,
      () => {
        if (sessionId !== undefined) followRoute(sessionId, fromSnapshot, nodeDid);
        else if (nodeDid) selectNode(nodeDid);
      },
      () =>
        navigate({
          name: "session",
          sessionId: previous.sessionId,
          ...(previous.nodeDid ? { nodeDid: previous.nodeDid } : {}),
        }),
    );
  }
  useEffect(() => {
    follow(false);
  }, [followRoute, selectNode, sessionId, nodeDid, unknownNode]);
  useEffect(() => {
    if (sessionId) noteOpenSession(sessionId);
  }, [sessionId]);
  useEffect(() => {
    if (sessionId) follow(true);
  }, [followRoute, sessionId, nodeDid, listed, unknownNode]);
}
