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
import { createSessionStore, holdsRequest } from "./sessionStore";
import { createProviderStore, type ProviderStore } from "./providerStore";
import { createProviders } from "./providers";
import { createLocalServer } from "./localServer";
import { createLocalServerStore, type LocalServerStore } from "./localServerStore";
import {
  createProvenance,
  createProvenanceStore,
  type ProvenanceStore,
} from "./provenance";
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
  const stores: ShellStores & {
    providers: ProviderStore;
    localServer: LocalServerStore;
    provenance: ProvenanceStore;
  } = {
    selection: createSelectionStore(),
    session: createSessionStore(),
    fleet: createFleetStore(),
    client: createClientStore(
      supportsManagedServer ? "checking-managed-server" : "loading-configuration",
    ),
    chat: createChatStore(),
    providers: createProviderStore(),
    localServer: createLocalServerStore(),
    provenance: createProvenanceStore(),
  };
  /** what the shell decides, kept in step with the stores */
  const view = createShellView(stores);
  const project = () => view.getState();
  followTranscript(stores, view);
  endHeldTurn(stores);
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
  const localServer = createLocalServer({
    api,
    store: stores.localServer,
    client: stores.client,
  });
  const lifecycle = createClientLifecycle({
    api,
    localServer,
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
    ...createProviders({ api, store: stores.providers, client: stores.client }),
    ...localServer,
    ...createProvenance({ api, stores }),
  };
  reconcileSelection(stores, actions.selectAgent);
  /* the composer's drafts, kept apart so a keystroke reaches only it */
  const drafts = createDraftStore();
  return { api, stores, view, drafts, project, trackedRequestId, lifecycle, actions };
}

export type DesktopApp = ReturnType<typeof createDesktopApp>;

/**
 * The app's copy of a sent message ends once the bridge holds the request,
 * whichever read shows it, so the copy is never drawn beside the row that
 * stands for it. Ended in the write that shows it, before anything renders.
 */
function endHeldTurn(stores: ShellStores) {
  /* the session's latest request as first read with the turn showing */
  let sent: { requestId: string; latestWhenSent: string | null } | null = null;
  const end = () => {
    const turn = stores.chat.getState().optimisticPendingTurn;
    if (!turn) return;
    const state = stores.session.getState();
    if (sent?.requestId !== turn.requestId) {
      if (state.session?.sessionId !== turn.sessionId) return;
      const latest = state.session.latestRequestId;
      sent = { requestId: turn.requestId, latestWhenSent: latest };
    }
    if (holdsRequest(state, { sessionId: turn.sessionId, ...sent }))
      chat.endPendingTurn(stores.chat, turn.requestId);
  };
  stores.session.subscribe(end);
  stores.chat.subscribe((state, prev) => {
    if (state.optimisticPendingTurn !== prev.optimisticPendingTurn) end();
  });
}

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
