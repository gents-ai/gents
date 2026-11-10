import { useMemo } from "react";
import {
  sessionGoalShown,
  type DesktopSessionSnapshot,
  type SessionGoalShown,
} from "@source-inc/gents-desktop-client";

import {
  heldFor,
  useSessionFields,
  useSessionValue,
  type SessionFacts,
  type SessionState,
} from "../../hooks/sessionStore";
import { useApp } from "../app/AppContext";

export function selectedIn(
  state: SessionState,
  {
    selectedSessionId,
    selectedNodeDid,
  }: { selectedSessionId: string | null; selectedNodeDid: string | null },
): DesktopSessionSnapshot | null {
  return heldFor(state.session, selectedSessionId, selectedNodeDid);
}

function useHeld() {
  const { stores } = useApp();
  const selection = {
    selectedSessionId: stores.selection.use.sessionId(),
    selectedNodeDid: stores.selection.use.nodeDid(),
  };
  return { store: stores.session, selection };
}

export function useSelectedSession() {
  const { store, selection } = useHeld();
  return useSessionValue(store, (state) => selectedIn(state, selection));
}

export function useSelectedSessionValue<T>(
  pick: (session: DesktopSessionSnapshot | null) => T,
): T {
  const { store, selection } = useHeld();
  return useSessionValue(store, (state) => pick(selectedIn(state, selection)));
}

export function useSelectedSessionFields<T extends object | null>(
  pick: (session: DesktopSessionSnapshot | null) => T,
): T {
  const { store, selection } = useHeld();
  return useSessionFields(store, (state) => pick(selectedIn(state, selection)));
}

export function useSessionFacts(): SessionFacts | null {
  const { store, selection } = useHeld();
  return useSessionValue(store, (state) =>
    selectedIn(state, selection) ? state.facts : null,
  );
}

/** What the open chat is told about its Goal (Lean `SessionObservation.shown`). */
export function useSelectedSessionGoal(): SessionGoalShown {
  const { store, selection } = useHeld();
  const held = useSessionValue(store, (state) =>
    selectedIn(state, selection) ? state.goal : null,
  );
  const sessionId = useSessionValue(
    store,
    (state) => selectedIn(state, selection)?.sessionId ?? null,
  );
  const nodeDid = useSessionValue(
    store,
    (state) => selectedIn(state, selection)?.nodeDid ?? null,
  );
  return useMemo(
    () =>
      sessionId ? sessionGoalShown(held, sessionId, nodeDid) : { kind: "pending" },
    [held, sessionId, nodeDid],
  );
}
