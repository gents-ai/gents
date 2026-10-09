/* Where the person left the lists: what the sessions list and the mailbox
   are narrowed to. It outlives the visit, in this browser's storage, so a
   narrowing is there when they come back. It names data (nodes, agents)
   that can come and go, so it is read back leniently and kept apart from
   the person's preferences. */
import { useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import {
  emptyFilter,
  restoreSessionFilter,
  type SessionFilter,
} from "@/lib/session-filter";
import { browserStorage, storedStrings } from "@/lib/storage";

export type ListViews = {
  sessionFilter: SessionFilter;
  /** the nodes the sessions list shows; null until the person picks, while
      it starts at the node this machine runs */
  sessionNodes: string[] | null;
  /** the nodes the mailbox shows; none picked is every node */
  mailboxNodes: string[];
  /** the mailbox's kinds; none picked is every kind */
  mailboxKinds: string[];
};

const initial = (): ListViews => ({
  sessionFilter: emptyFilter,
  sessionNodes: null,
  mailboxNodes: [],
  mailboxKinds: [],
});

/** What storage held, as list views: storage is outside the app's control
    (another build, a hand edit), so a value that is not one is the default. */
export function restoreListViews(stored: unknown): ListViews {
  const saved = (typeof stored === "object" && stored !== null ? stored : {}) as Record<
    string,
    unknown
  >;
  return {
    sessionFilter: restoreSessionFilter(saved.sessionFilter),
    sessionNodes: Array.isArray(saved.sessionNodes)
      ? storedStrings(saved.sessionNodes)
      : null,
    mailboxNodes: storedStrings(saved.mailboxNodes),
    mailboxKinds: storedStrings(saved.mailboxKinds),
  };
}

const store = createStore<ListViews>()(
  persist(initial, {
    name: "gents-list-views",
    version: 1,
    storage: createJSONStorage(() => browserStorage),
    merge: (stored) => restoreListViews(stored),
  }),
);

/** The person's changes to how the lists are narrowed. */
export const listViews = {
  setSessionFilter(sessionFilter: SessionFilter) {
    store.setState({ sessionFilter });
  },
  /** null lets the sessions list start at the node this machine runs again */
  setSessionNodes(sessionNodes: string[] | null) {
    store.setState({ sessionNodes });
  },
  setMailboxNodes(mailboxNodes: string[]) {
    store.setState({ mailboxNodes });
  },
  setMailboxKinds(mailboxKinds: string[]) {
    store.setState({ mailboxKinds });
  },
};

/** A value from the list views; re-renders when it changes. */
export function useListViews<T>(select: (views: ListViews) => T): T {
  return useStore(store, select);
}

/** tests only: the lists as on a first run */
export const resetListViews = () => store.setState(initial(), true);
