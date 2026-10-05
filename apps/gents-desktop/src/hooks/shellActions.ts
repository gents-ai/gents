import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";

import { createDesktopShellChatActions } from "./desktopShellChatActions";
import { createDesktopShellConfigActions } from "./desktopShellConfigActions";
import { createDesktopShellMailboxActions } from "./desktopShellMailboxActions";
import { createDesktopShellPeerActions } from "./desktopShellPeerActions";
import { createDesktopShellSelectionActions } from "./desktopShellSelectionActions";
import { createDesktopShellTaskActions } from "./desktopShellTaskActions";
import { selection } from "./selectionStore";
import type { createSessionReads } from "./sessionReads";
import type { ShellProjection, ShellStores } from "./shellProjection";

type ShellActionParams = {
  api: DesktopApiAdapter;
  stores: ShellStores;
  /** the shell as the stores hold it now */
  project: () => ShellProjection;
  reads: ReturnType<typeof createSessionReads>;
  /** the client's reads and its single-flight start */
  client: {
    refreshSnapshot: () => Promise<void>;
    mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
    ensureDesktopClientStarted: () => Promise<DesktopClientSnapshot>;
  };
  /** shows a failed action once; null clears nothing a toast holds */
  setError: (error: string | null) => void;
};

/**
 * Everything the person can do, made once for the app's life. Each action
 * reads the stores when it runs, so its identity never changes and it never
 * acts on a stale copy.
 */
export function createShellActions({
  api,
  stores,
  project,
  reads,
  client: { refreshSnapshot, mutateSnapshot, ensureDesktopClientStarted },
  setError,
}: ShellActionParams) {
  const route = createDesktopShellSelectionActions({ stores, setError });
  return {
    ...reads,
    ...route,
    refreshSnapshot,
    ...createDesktopShellMailboxActions({ api, stores, refreshSnapshot, setError }),
    clearMailboxCause: () => selection.releaseMailboxRoute(stores.selection),
    ...createDesktopShellPeerActions({
      api,
      stores,
      ensureDesktopClientStarted,
      mutateSnapshot,
      refreshSnapshot,
      setError,
      selectAgent: route.selectAgent,
    }),
    ...createDesktopShellConfigActions({ api, mutateSnapshot, setError }),
    ...createDesktopShellTaskActions({
      api,
      store: stores.selection,
      refreshSnapshot,
      setError,
    }),
    ...createDesktopShellChatActions({
      api,
      stores,
      project,
      refreshSession: reads.refreshSession,
      refreshSnapshot,
      setError,
    }),
    captureComposeIntent: () => selection.captureIntent(stores.selection),
    acceptsComposeIntent: (captured: number) =>
      selection.acceptsIntent(stores.selection, captured),
  };
}

export type ShellActions = ReturnType<typeof createShellActions>;
