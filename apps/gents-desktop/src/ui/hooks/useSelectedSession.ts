import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";

import {
  useSessionFields,
  useSessionValue,
  type SessionFacts,
  type SessionState,
  type SessionStore,
} from "../../hooks/sessionStore";

/** What a screen needs to find the selected session in the session store. */
export type SessionSelection = {
  sessionStore: SessionStore;
  selectedSessionId: string | null;
  selectedAgentDid: string | null;
};

/** The held session when it is the selected one, else null: a read for the
    previous selection can still be held while the next one loads. */
export function selectedIn(
  state: SessionState,
  { selectedSessionId, selectedAgentDid }: Omit<SessionSelection, "sessionStore">,
): DesktopSessionSnapshot | null {
  const session = state.session;
  return session?.sessionId === selectedSessionId &&
    (!selectedAgentDid || !session.agentDid || session.agentDid === selectedAgentDid)
    ? session
    : null;
}

/** The selected session; re-renders the caller on every change, streamed
    chunks included. Only the transcript reads it whole. */
export function useSelectedSession(shell: SessionSelection) {
  return useSessionValue(shell.sessionStore, (state) => selectedIn(state, shell));
}

/** A value from the selected session; re-renders when it changes by identity. */
export function useSelectedSessionValue<T>(
  shell: SessionSelection,
  pick: (session: DesktopSessionSnapshot | null) => T,
): T {
  return useSessionValue(shell.sessionStore, (state) => pick(selectedIn(state, shell)));
}

/** Fields of the selected session; re-renders when any of them changes. */
export function useSelectedSessionFields<T extends object | null>(
  shell: SessionSelection,
  pick: (session: DesktopSessionSnapshot | null) => T,
): T {
  return useSessionFields(shell.sessionStore, (state) =>
    pick(selectedIn(state, shell)),
  );
}

/** The selected session's transcript facts, or null while it is not held. */
export function useSessionFacts(shell: SessionSelection): SessionFacts | null {
  return useSessionValue(shell.sessionStore, (state) =>
    selectedIn(state, shell) ? state.facts : null,
  );
}
