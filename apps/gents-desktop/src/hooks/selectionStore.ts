import { useStore } from "zustand";
import { createStore, type StoreApi } from "zustand/vanilla";

import { acceptsAsyncResult } from "./desktopShellRuntime";

/** What the person is looking at: a node, a session on it (null for a new
    one), and the behavior a message goes to. */
export type Selection = {
  agentDid: string | null;
  sessionId: string | null;
  behaviorId: string | null;
};

/** A mailbox item opened for a reply: the selection it set up, and the item
    the next message answers. */
export type MailboxRoute = {
  itemId: string;
  agentDid: string;
  behaviorId: string;
  sessionId: string | null;
};

export type SelectionState = Selection & {
  /** held while the selection is still the one the item set up */
  mailboxRoute: MailboxRoute | null;
  /** the node a new session is being composed for, from the new-session
      screen or a mailbox item; its behavior is the person's, so snapshot
      reconciliation leaves it alone */
  composingFor: string | null;
  /** advanced by every navigation: an async result captured under an older
      intent arrived after the person moved on, and is dropped */
  intent: number;
};

export type SelectionStore = StoreApi<SelectionState>;

const EMPTY: SelectionState = {
  agentDid: null,
  sessionId: null,
  behaviorId: null,
  mailboxRoute: null,
  composingFor: null,
  intent: 0,
};

export function createSelectionStore(initial: Partial<SelectionState> = {}) {
  return createStore<SelectionState>(() => ({ ...EMPTY, ...initial }));
}

/* A mailbox route lasts while the selection is the one it set up. Checked
   on every write, so no change of node, session or behavior, from any
   source, can carry the item into a message it was not opened for. */
function commit(store: SelectionStore, next: Partial<SelectionState>) {
  store.setState((state) => {
    const merged = { ...state, ...next };
    const route = merged.mailboxRoute;
    const left =
      route !== null &&
      (route.agentDid !== merged.agentDid ||
        route.behaviorId !== merged.behaviorId ||
        route.sessionId !== merged.sessionId);
    return left ? { ...merged, mailboxRoute: null, composingFor: null } : merged;
  });
}

const advanced = (store: SelectionStore) => store.getState().intent + 1;

/** The selection's changes. Each says whether the person navigated, which
    advances the intent and lets go of a mailbox route. */
export const selection = {
  /** another node: its session and behavior start over. Returns whether the
      node changed, so the caller can drop the session it showed. */
  selectAgent(store: SelectionStore, agentDid: string | null): boolean {
    const changed = agentDid !== store.getState().agentDid;
    if (changed)
      commit(store, {
        agentDid,
        sessionId: null,
        behaviorId: null,
        mailboxRoute: null,
        composingFor: null,
        intent: advanced(store),
      });
    return changed;
  },

  /** a session on the selected node, with the behavior it was held under
      when the node lists it */
  selectSession(store: SelectionStore, sessionId: string, behaviorId?: string | null) {
    commit(store, {
      sessionId,
      ...(behaviorId ? { behaviorId } : {}),
      mailboxRoute: null,
      composingFor: null,
      intent: advanced(store),
    });
  },

  /** a session and its node together, for a route naming a session on a
      node that is not selected */
  selectSessionOn(
    store: SelectionStore,
    agentDid: string,
    sessionId: string,
    behaviorId?: string | null,
  ) {
    commit(store, {
      agentDid,
      sessionId,
      behaviorId: behaviorId ?? null,
      mailboxRoute: null,
      composingFor: null,
      intent: advanced(store),
    });
  },

  selectBehavior(store: SelectionStore, behaviorId: string | null) {
    if (behaviorId === store.getState().behaviorId) return;
    commit(store, {
      behaviorId,
      mailboxRoute: null,
      composingFor: null,
      intent: advanced(store),
    });
  },

  /** the new-session screen for a node and behavior */
  startNewSession(store: SelectionStore, agentDid: string, behaviorId: string | null) {
    commit(store, {
      sessionId: null,
      behaviorId,
      mailboxRoute: null,
      composingFor: agentDid,
      intent: advanced(store),
    });
  },

  /** a mailbox item opened for a reply: the selection it names, held for
      the next message while nothing moves it */
  openMailboxRoute(store: SelectionStore, route: MailboxRoute) {
    commit(store, {
      agentDid: route.agentDid,
      behaviorId: route.behaviorId,
      sessionId: route.sessionId,
      mailboxRoute: route,
      composingFor: route.agentDid,
    });
  },

  /** the item was answered, dismissed or put down: the next message is an
      ordinary one */
  releaseMailboxRoute(store: SelectionStore) {
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

  /** the behavior a snapshot settles for the selected node; not a navigation */
  settleBehavior(store: SelectionStore, behaviorId: string | null) {
    if (behaviorId !== store.getState().behaviorId) commit(store, { behaviorId });
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
