import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";

import { createSessionStore } from "../src/hooks/sessionStore";

export function selectedSessionFields(session: DesktopSessionSnapshot | null) {
  return {
    sessionStore: createSessionStore(session),
    selectedSessionId: session?.sessionId ?? null,
    selectedNodeDid: session?.nodeDid ?? null,
  };
}
