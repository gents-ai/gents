import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";

import {
  heldFor,
  useSessionFields,
  useSessionValue,
  type SessionFacts,
  type SessionState,
} from "../../hooks/sessionStore";
import { useApp } from "../app/AppContext";

/** The held session when it is the selected one (heldFor), else null. */
export function selectedIn(
  state: SessionState,
  {
    selectedSessionId,
    selectedAgentDid,
  }: { selectedSessionId: string | null; selectedAgentDid: string | null },
): DesktopSessionSnapshot | null {
  return heldFor(state.session, selectedSessionId, selectedAgentDid);
}

/* the session store and the selection that picks the held read out of it */
function useHeld() {
  const { stores } = useApp();
  const selection = {
    selectedSessionId: stores.selection.use.sessionId(),
    selectedAgentDid: stores.selection.use.agentDid(),
  };
  return { store: stores.session, selection };
}

/** The selected session; re-renders the caller on every change, streamed
    chunks included. Only the transcript reads it whole. */
export function useSelectedSession() {
  const { store, selection } = useHeld();
  return useSessionValue(store, (state) => selectedIn(state, selection));
}

/** A value from the selected session; re-renders when it changes by identity. */
export function useSelectedSessionValue<T>(
  pick: (session: DesktopSessionSnapshot | null) => T,
): T {
  const { store, selection } = useHeld();
  return useSessionValue(store, (state) => pick(selectedIn(state, selection)));
}

/** Fields of the selected session; re-renders when any of them changes. */
export function useSelectedSessionFields<T extends object | null>(
  pick: (session: DesktopSessionSnapshot | null) => T,
): T {
  const { store, selection } = useHeld();
  return useSessionFields(store, (state) => pick(selectedIn(state, selection)));
}

/** The selected session's transcript facts, or null while it is not held. */
export function useSessionFacts(): SessionFacts | null {
  const { store, selection } = useHeld();
  return useSessionValue(store, (state) =>
    selectedIn(state, selection) ? state.facts : null,
  );
}
