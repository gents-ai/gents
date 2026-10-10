import { useStore } from "zustand";
import { createStore, type StoreApi } from "zustand/vanilla";

import { createSelectors, type WithSelectors } from "./createSelectors";
import { acceptsAsyncResult } from "./observationOrdering";

/** What the person is looking at: a node, a session on it (null for a new
    one), and the agent a message goes to. */
export type Selection = {
  nodeDid: string | null;
  sessionId: string | null;
  agentId: string | null;
};

/** A mailbox item opened for a reply: the selection it set up, and the item
    the next message answers. */
export type MailboxRoute = {
  itemId: string;
  nodeDid: string;
  agentId: string;
  sessionId: string | null;
};

export type SelectionState = Selection & {
  /** held while the selection is still the one the item set up */
  mailboxRoute: MailboxRoute | null;
  /** the node a new session is being composed for, from the new-session
      screen or a mailbox item; its agent is the person's, so snapshot
      reconciliation leaves it alone */
  composingFor: string | null;
  /** advanced by every navigation: an async result captured under an older
      intent arrived after the person moved on, and is dropped */
  intent: number;
};

export type SelectionStore = WithSelectors<StoreApi<SelectionState>>;

const EMPTY: SelectionState = {
  nodeDid: null,
  sessionId: null,
  agentId: null,
  mailboxRoute: null,
  composingFor: null,
  intent: 0,
};

export function createSelectionStore(initial: Partial<SelectionState> = {}) {
  return createSelectors(createStore<SelectionState>(() => ({ ...EMPTY, ...initial })));
}

/* A mailbox route lasts while the selection is the one it set up. Checked
   on every write, so no change of node, session or agent, from any
   source, can carry the item into a message it was not opened for. */
function commit(store: SelectionStore, next: Partial<SelectionState>) {
  store.setState((state) => {
    const merged = { ...state, ...next };
    const route = merged.mailboxRoute;
    const left =
      route !== null &&
      (route.nodeDid !== merged.nodeDid ||
        route.agentId !== merged.agentId ||
        route.sessionId !== merged.sessionId);
    return left ? { ...merged, mailboxRoute: null, composingFor: null } : merged;
  });
}

const advanced = (store: SelectionStore) => store.getState().intent + 1;

/** The selection's changes. Each says whether the person navigated, which
    advances the intent and lets go of a mailbox route. */
export const selection = {
  /** another node: its session and agent start over. Returns whether the
      node changed, so the caller can drop the session it showed. */
  selectNode(store: SelectionStore, nodeDid: string | null): boolean {
    const changed = nodeDid !== store.getState().nodeDid;
    if (changed)
      commit(store, {
        nodeDid,
        sessionId: null,
        agentId: null,
        mailboxRoute: null,
        composingFor: null,
        intent: advanced(store),
      });
    return changed;
  },

  /** a session on the selected node, with the agent it was held under
      when the node lists it */
  selectSession(store: SelectionStore, sessionId: string, agentId?: string | null) {
    commit(store, {
      sessionId,
      ...(agentId ? { agentId } : {}),
      mailboxRoute: null,
      composingFor: null,
      intent: advanced(store),
    });
  },

  /** a session and its node together, for a route naming a session on a
      node that is not selected */
  selectSessionOn(
    store: SelectionStore,
    nodeDid: string,
    sessionId: string,
    agentId?: string | null,
  ) {
    commit(store, {
      nodeDid,
      sessionId,
      agentId: agentId ?? null,
      mailboxRoute: null,
      composingFor: null,
      intent: advanced(store),
    });
  },

  selectAgent(store: SelectionStore, agentId: string | null) {
    if (agentId === store.getState().agentId) return;
    commit(store, {
      agentId,
      mailboxRoute: null,
      composingFor: null,
      intent: advanced(store),
    });
  },

  /** the new-session screen for a node and agent */
  startNewSession(store: SelectionStore, nodeDid: string, agentId: string | null) {
    commit(store, {
      sessionId: null,
      agentId,
      mailboxRoute: null,
      composingFor: nodeDid,
      intent: advanced(store),
    });
  },

  /** a mailbox item opened for a reply: the selection it names, held for
      the next message while nothing moves it */
  openMailboxRoute(store: SelectionStore, route: MailboxRoute) {
    commit(store, {
      nodeDid: route.nodeDid,
      agentId: route.agentId,
      sessionId: route.sessionId,
      mailboxRoute: route,
      composingFor: route.nodeDid,
    });
  },

  /** the item was answered, dismissed or put down: the next message is an
      ordinary one. Given an item, only a route opened for that item is let
      go, so one opened since for another item stays. */
  releaseMailboxRoute(store: SelectionStore, itemId?: string) {
    const route = store.getState().mailboxRoute;
    if (itemId !== undefined && route?.itemId !== itemId) return;
    commit(store, { mailboxRoute: null, composingFor: null });
  },

  /** a send created or continued this session: it is now the one selected,
      and whatever composed it is done */
  adoptSession(store: SelectionStore, sessionId: string) {
    commit(store, { sessionId, mailboxRoute: null, composingFor: null });
  },

  /** the session a retried request now runs in; not a navigation */
  settleSession(store: SelectionStore, sessionId: string) {
    commit(store, { sessionId });
  },

  /** the agent a snapshot settles for the selected node; not a navigation */
  settleAgent(store: SelectionStore, agentId: string | null) {
    if (agentId !== store.getState().agentId) commit(store, { agentId });
  },

  /** a navigation that changes no selection, such as opening a mailbox item
      before the bridge answers */
  advanceIntent(store: SelectionStore) {
    commit(store, { intent: advanced(store) });
  },

  /** the intent now, to check an async result against later */
  captureIntent: (store: SelectionStore) => store.getState().intent,

  /** whether the person is still where they were when `captured` was taken */
  acceptsIntent: (store: SelectionStore, captured: number) =>
    acceptsAsyncResult(store.getState().intent, captured),
};

/** The selection, re-rendering the caller when it changes. */
export function useSelection(store: SelectionStore): SelectionState {
  return useStore(store);
}
