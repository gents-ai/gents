import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";

import { createDesktopShellChatActions } from "./desktopShellChatActions";
import { createDesktopShellConfigActions } from "./desktopShellConfigActions";
import { createDesktopShellHostActions } from "./desktopShellHostActions";
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
  const route = createDesktopShellSelectionActions({ stores });
  return {
    ...reads,
    ...route,
    /** Reads the client again and publishes it to the stores; only the
        newest read issued publishes. A failure is the client's own state,
        shown in the banner. */
    refreshSnapshot,
    ...createDesktopShellMailboxActions({
      api,
      stores,
      refreshSnapshot,
      reportFailure,
    }),
    /** Puts down the mailbox item the next message was going to answer; the
        item stays open, and the next message is an ordinary one. */
    clearMailboxCause: () => selection.releaseMailboxRoute(stores.selection),
    ...createDesktopShellPeerActions({
      api,
      stores,
      ensureDesktopClientStarted,
      mutateSnapshot,
      refreshSnapshot,
      reportFailure,
      selectAgent: route.selectAgent,
    }),
    ...createDesktopShellConfigActions({ api, mutateSnapshot, reportFailure }),
    ...createDesktopShellHostActions({ api }),
    ...createDesktopShellTaskActions({
      api,
      store: stores.selection,
      refreshSnapshot,
      reportFailure,
    }),
    ...createDesktopShellChatActions({
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
