import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";

import { createSessionStore } from "../src/hooks/sessionStore";

/** A shell's selected session: the session store holding `session`, and the
    selection that picks it out. Spread into a test shell. */
export function selectedSessionFields(session: DesktopSessionSnapshot | null) {
  return {
    sessionStore: createSessionStore(session),
    selectedSessionId: session?.sessionId ?? null,
    selectedNodeDid: session?.nodeDid ?? null,
  };
}
