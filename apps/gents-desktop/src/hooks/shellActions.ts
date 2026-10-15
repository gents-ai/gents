import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopOperationsSnapshotRequest,
} from "@source-inc/gents-desktop-client";

import { createChatActions } from "./chatActions";
import { createConfigActions } from "./configActions";
import { createHostActions } from "./hostActions";
import { createMailboxActions } from "./mailboxActions";
import { createPeerActions } from "./peerActions";
import { createSelectionActions } from "./selectionActions";
import { createTaskActions } from "./taskActions";
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
  /** shows a failed action to the person, once */
  reportFailure: (message: string) => void;
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
  reportFailure,
}: ShellActionParams) {
  const route = createSelectionActions({ stores });
  return {
    ...reads,
    ...route,
    /** Reads the client again and publishes it to the stores; only the
        newest read issued publishes. A failure is the client's own state,
        shown in the banner. */
    refreshSnapshot,
    /** Reads the client without publishing it, for a screen checking what
        its own write left behind. */
    readSnapshot: () => api.fetchDesktopSnapshot(),
    /* Read for the screen that shows each answer, which keeps it: no store. */
    fetchOperationsSnapshot: (request: DesktopOperationsSnapshotRequest) =>
      api.fetchOperationsSnapshot(request),
    /** Whether this host has a DB explorer window to open. */
    canOpenDbExplorer: Boolean(api.openDbExplorer),
    /** Opens the managed runtime's DB explorer window. */
    openDbExplorer: async () => {
      await api.openDbExplorer?.();
    },
    ...createMailboxActions({
      api,
      stores,
      refreshSnapshot,
      reportFailure,
    }),
    /** Puts down the mailbox item the next message was going to answer; the
        item stays open, and the next message is an ordinary one. */
    clearMailboxCause: () => selection.releaseMailboxRoute(stores.selection),
    ...createPeerActions({
      api,
      stores,
      ensureDesktopClientStarted,
      mutateSnapshot,
      refreshSnapshot,
      reportFailure,
      selectNode: route.selectNode,
    }),
    ...createConfigActions({ api, mutateSnapshot, reportFailure }),
    ...createHostActions({ api }),
    ...createTaskActions({
      api,
      store: stores.selection,
      refreshSnapshot,
      reportFailure,
    }),
    ...createChatActions({
      api,
      stores,
      project,
      refreshSession: reads.refreshSession,
      refreshSnapshot,
      reportFailure,
    }),
    /** Where the person is now, to check an async result against later
        with acceptsComposeIntent. */
    captureComposeIntent: () => selection.captureIntent(stores.selection),
    /** Whether the person is still where they were when `captured` was
        taken; a result from after they moved on is dropped. */
    acceptsComposeIntent: (captured: number) =>
      selection.acceptsIntent(stores.selection, captured),
  };
}

export type ShellActions = ReturnType<typeof createShellActions>;
