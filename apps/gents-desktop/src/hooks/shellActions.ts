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
  project: () => ShellProjection;
  reads: ReturnType<typeof createSessionReads>;
  client: {
    refreshSnapshot: () => Promise<void>;
    mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
    ensureDesktopClientStarted: () => Promise<DesktopClientSnapshot>;
  };
  reportFailure: (message: string) => void;
};

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
    refreshSnapshot,
    readSnapshot: () => api.fetchDesktopSnapshot(),
    fetchOperationsSnapshot: (request: DesktopOperationsSnapshotRequest) =>
      api.fetchOperationsSnapshot(request),
    canOpenDbExplorer: Boolean(api.openDbExplorer),
    openDbExplorer: async () => {
      await api.openDbExplorer?.();
    },
    ...createMailboxActions({
      api,
      stores,
      refreshSnapshot,
      reportFailure,
    }),
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
    captureComposeIntent: () => selection.captureIntent(stores.selection),
    acceptsComposeIntent: (captured: number) =>
      selection.acceptsIntent(stores.selection, captured),
  };
}

export type ShellActions = ReturnType<typeof createShellActions>;
