import type {
  DesktopApiAdapter,
  DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";

import { chat, createChatStore, type ChatState } from "./chatStore";
import { createClientLifecycle } from "./clientLifecycle";
import { clientStatus, createClientStore } from "./clientStore";
import { createFleetStore } from "./fleetStore";
import { createSelectionStore } from "./selectionStore";
import { reconcileSelection } from "./selectionReconcile";
import { createSessionReads } from "./sessionReads";
import { createSessionStore } from "./sessionStore";
import { createProviderStore, type ProviderStore } from "./providerStore";
import { createProviderReads } from "./providerReads";
import { createShellActions } from "./shellActions";
import { createDraftStore } from "./draftStore";
import type { ShellStores } from "./shellProjection";
import { createShellView, type ShellViewStore } from "./shellView";

/** What the host gives the app: the bridge's API and its update events. */
export type DesktopBridge = {
  api: DesktopApiAdapter;
  listenToUpdates: DesktopClientUpdatedListenerFactory;
  supportsManagedServer?: boolean;
  /** shows a failed action to the person, once; the app passes its toast */
  reportFailure?: (message: string) => void;
};

export type DesktopAppParams = Omit<DesktopBridge, "listenToUpdates">;

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
  const stores: ShellStores & { providers: ProviderStore } = {
    selection: createSelectionStore(),
    session: createSessionStore(),
    fleet: createFleetStore(),
    client: createClientStore(
      supportsManagedServer ? "checking-managed-server" : "loading-configuration",
    ),
    chat: createChatStore(),
    providers: createProviderStore(),
  };
  /** what the shell decides, kept in step with the stores */
  const view = createShellView(stores);
  const project = () => view.getState();
  followTranscript(stores, view);
  /** the request being tracked now, read when an update or a read lands */
  const trackedRequestId = () => project().trackedRequestId;
  /* A failed action is reported once, as a toast, by the action itself: it
     happened where the person clicked and is over. Only the client's own
     state belongs in the banner: the lifecycle, the session reads and the
     effects that refresh in the background, so a repeated poll failure does
     not raise a toast every interval. */
  const reportAction = (message: string) => reportFailure?.(message);
  const reads = createSessionReads({
    api,
    store: stores.selection,
    sessionStore: stores.session,
    trackedRequestId,
    setError: (error) => clientStatus.setError(stores.client, error),
  });
  const lifecycle = createClientLifecycle({
    api,
    supportsManagedServer,
    stores,
    refreshSession: reads.refreshSession,
  });
  const actions = {
    ...createShellActions({
      api,
      stores,
      project,
      reads,
      client: lifecycle,
      reportFailure: reportAction,
    }),
    ...createProviderReads({ api, store: stores.providers, client: stores.client }),
  };
  reconcileSelection(stores, actions.selectAgent);
  /* the composer's drafts, kept apart so a keystroke reaches only it */
  const drafts = createDraftStore();
  return { api, stores, view, drafts, project, trackedRequestId, lifecycle, actions };
}

export type DesktopApp = ReturnType<typeof createDesktopApp>;

/**
 * The local workflow follows what the transcript shows once it shows it,
 * and a submission whose send has ended is released. Reactions between
 * stores, so they run as the stores change; the send writes its workflow
 * and its sending flag together, so no state between them is seen.
 */
function followTranscript(stores: ShellStores, view: ShellViewStore) {
  view.subscribe((state, prev) => {
    const projected = state.shellProjection.workflow;
    if (projected !== prev.shellProjection.workflow)
      chat.followProjection(stores.chat, projected);
  });
  const ended = (state: ChatState) =>
    state.localWorkflow.kind === "submittingRequest" && !state.sending;
  stores.chat.subscribe((state, prev) => {
    if (ended(state) && !ended(prev)) chat.resetWorkflow(stores.chat);
  });
}
