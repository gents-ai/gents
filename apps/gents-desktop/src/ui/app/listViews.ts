// Outlives the visit in this browser's storage; it names data (nodes,
// agents) that can come and go, so it is read back leniently.
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
  sessionNodes: string[] | null;
  mailboxNodes: string[];
  mailboxKinds: string[];
};

const initial = (): ListViews => ({
  sessionFilter: emptyFilter,
  sessionNodes: null,
  mailboxNodes: [],
  mailboxKinds: [],
});

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

// Tests only: the lists as on a first run.
export const resetListViews = () => store.setState(initial(), true);
