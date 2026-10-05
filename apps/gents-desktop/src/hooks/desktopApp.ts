import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

import { createChatStore } from "./chatStore";
import { createClientLifecycle } from "./clientLifecycle";
import { clientSetter, createClientStore } from "./clientStore";
import { createFleetStore } from "./fleetStore";
import { createSelectionStore } from "./selectionStore";
import { createSessionReads } from "./sessionReads";
import { createSessionStore } from "./sessionStore";
import { createShellActions } from "./shellActions";
import { projectShell, projectionInputsOf, type ShellStores } from "./shellProjection";

export type DesktopAppParams = {
  api: DesktopApiAdapter;
  supportsManagedServer?: boolean;
  /** shows a failed action to the person, once; the app passes its toast */
  reportFailure?: (message: string) => void;
};

/**
 * The desktop app outside React: its stores, the client's lifecycle and
 * every action, made once for the app's life. Screens select from the
 * stores and call the actions; nothing here holds a render's copy of
 * anything.
 */
export function createDesktopApp({
  api,
  supportsManagedServer = false,
  reportFailure,
}: DesktopAppParams) {
  const stores: ShellStores = {
    selection: createSelectionStore(),
    session: createSessionStore(),
    fleet: createFleetStore(),
    client: createClientStore(
      supportsManagedServer ? "checking-managed-server" : "loading-configuration",
    ),
    chat: createChatStore(),
  };
  /** the projection as the stores hold it now */
  const project = () => projectShell(projectionInputsOf(stores));
  /** the request being tracked now, read when an update or a read lands */
  const trackedRequestId = () => project().trackedRequestId;
  /* A failed action is reported once, as a toast, by the action itself: it
     happened where the person clicked and is over. Only the client's own
     state belongs in the banner: the lifecycle, the session reads and the
     effects that refresh in the background, so a repeated poll failure does
     not raise a toast every interval. Actions clear an earlier error with
     null; a toast has nothing to clear. */
  const reportAction = (message: string | null) => {
    if (message) reportFailure?.(message);
  };
  const reads = createSessionReads({
    api,
    store: stores.selection,
    sessionStore: stores.session,
    trackedRequestId,
    setError: clientSetter(stores.client, "error"),
  });
  const lifecycle = createClientLifecycle({
    api,
    supportsManagedServer,
    stores,
    refreshSession: reads.refreshSession,
  });
  const actions = createShellActions({
    api,
    stores,
    project,
    reads,
    client: lifecycle,
    setError: reportAction,
  });
  return { api, stores, project, trackedRequestId, lifecycle, actions };
}

export type DesktopApp = ReturnType<typeof createDesktopApp>;
