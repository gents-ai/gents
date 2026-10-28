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
import { observeMailboxAnswerRetirement } from "./mailboxAnswerRetirement";
import { seenMarks } from "../ui/app/sessionViews";
import { seenMarkOf } from "../ui/lib/seen";
import { listedSession, sessionKeyOf } from "./fleetStore";

export type DesktopBridge = {
  api: DesktopApiAdapter;
  listenToUpdates: DesktopClientUpdatedListenerFactory;
  supportsManagedServer?: boolean;
  reportFailure?: (message: string) => void;
};

export type DesktopAppParams = Omit<DesktopBridge, "listenToUpdates">;

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
  const view = createShellView(stores);
  const project = () => view.getState();
  followTranscript(stores, view);
  endHeldTurn(stores);
  const trackedRequestId = () => project().trackedRequestId;
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
  observeMailboxAnswerRetirement({ api, stores });
  reconcileSelection(stores, actions.selectNode);
  keepSeen(stores);
  const drafts = createDraftStore();
  return { api, stores, view, drafts, project, trackedRequestId, lifecycle, actions };
}

export type DesktopApp = ReturnType<typeof createDesktopApp>;

/* the open session is seen while the window shows it, so work that ends
   in front of the reader never reads as new */
function keepSeen(stores: ShellStores) {
  seenMarks.start(new Date().toISOString());
  const stamp = () => {
    if (typeof document !== "undefined" && document.visibilityState === "hidden")
      return;
    const { nodeDid, sessionId } = stores.selection.getState();
    const session = listedSession(stores.fleet.getState(), nodeDid, sessionId);
    if (session) seenMarks.set(sessionKeyOf(session), seenMarkOf(session));
  };
  stores.selection.subscribe(stamp);
  stores.fleet.subscribe(stamp);
  if (typeof document !== "undefined")
    document.addEventListener("visibilitychange", stamp);
  stamp();
}

function endHeldTurn(stores: ShellStores) {
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
