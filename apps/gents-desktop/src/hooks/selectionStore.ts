import { useStore } from "zustand";
import { createStore, type StoreApi } from "zustand/vanilla";

import { createSelectors, type WithSelectors } from "./createSelectors";
import { acceptsAsyncResult } from "./observationOrdering";

export type Selection = {
  nodeDid: string | null;
  sessionId: string | null;
  agentDid: string | null;
};

export type MailboxRoute = {
  itemId: string;
  nodeDid: string;
  agentDid: string;
  sessionId: string | null;
};

export type SelectionState = Selection & {
  mailboxRoute: MailboxRoute | null;
  composingFor: string | null;
  intent: number;
};

export type SelectionStore = WithSelectors<StoreApi<SelectionState>>;

const EMPTY: SelectionState = {
  nodeDid: null,
  sessionId: null,
  agentDid: null,
  mailboxRoute: null,
  composingFor: null,
  intent: 0,
};

export function createSelectionStore(initial: Partial<SelectionState> = {}) {
  return createSelectors(createStore<SelectionState>(() => ({ ...EMPTY, ...initial })));
}

function commit(store: SelectionStore, next: Partial<SelectionState>) {
  store.setState((state) => {
    const merged = { ...state, ...next };
    const route = merged.mailboxRoute;
    const left =
      route !== null &&
      (route.nodeDid !== merged.nodeDid ||
        route.agentDid !== merged.agentDid ||
        route.sessionId !== merged.sessionId);
    return left ? { ...merged, mailboxRoute: null, composingFor: null } : merged;
  });
}

const advanced = (store: SelectionStore) => store.getState().intent + 1;

export const selection = {
  selectNode(store: SelectionStore, nodeDid: string | null): boolean {
    const changed = nodeDid !== store.getState().nodeDid;
    if (changed)
      commit(store, {
        nodeDid,
        sessionId: null,
        agentDid: null,
        mailboxRoute: null,
        composingFor: null,
        intent: advanced(store),
      });
    return changed;
  },

  selectSession(store: SelectionStore, sessionId: string, agentDid?: string | null) {
    commit(store, {
      sessionId,
      ...(agentDid ? { agentDid } : {}),
      mailboxRoute: null,
      composingFor: null,
      intent: advanced(store),
    });
  },

  selectSessionOn(
    store: SelectionStore,
    nodeDid: string,
    sessionId: string,
    agentDid?: string | null,
  ) {
    commit(store, {
      nodeDid,
      sessionId,
      agentDid: agentDid ?? null,
      mailboxRoute: null,
      composingFor: null,
      intent: advanced(store),
    });
  },

  selectAgent(store: SelectionStore, agentDid: string | null) {
    if (agentDid === store.getState().agentDid) return;
    commit(store, {
      agentDid,
      mailboxRoute: null,
      composingFor: null,
      intent: advanced(store),
    });
  },

  startNewSession(store: SelectionStore, nodeDid: string, agentDid: string | null) {
    commit(store, {
      nodeDid,
      sessionId: null,
      agentDid,
      mailboxRoute: null,
      composingFor: nodeDid,
      intent: advanced(store),
    });
  },

  openMailboxRoute(store: SelectionStore, route: MailboxRoute) {
    commit(store, {
      nodeDid: route.nodeDid,
      agentDid: route.agentDid,
      sessionId: route.sessionId,
      mailboxRoute: route,
      composingFor: route.nodeDid,
    });
  },

  releaseMailboxRoute(store: SelectionStore, itemId?: string) {
    const route = store.getState().mailboxRoute;
    if (itemId !== undefined && route?.itemId !== itemId) return;
    commit(store, { mailboxRoute: null, composingFor: null });
  },

  adoptSession(store: SelectionStore, sessionId: string) {
    commit(store, { sessionId, mailboxRoute: null, composingFor: null });
  },

  settleSession(store: SelectionStore, sessionId: string) {
    commit(store, { sessionId });
  },

  settleAgent(store: SelectionStore, agentDid: string | null) {
    if (agentDid !== store.getState().agentDid) commit(store, { agentDid });
  },

  advanceIntent(store: SelectionStore) {
    commit(store, { intent: advanced(store) });
  },

  captureIntent: (store: SelectionStore) => store.getState().intent,

  acceptsIntent: (store: SelectionStore, captured: number) =>
    acceptsAsyncResult(store.getState().intent, captured),
};

export function useSelection(store: SelectionStore): SelectionState {
  return useStore(store);
}
