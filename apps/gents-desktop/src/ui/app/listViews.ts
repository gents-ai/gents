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
  /* the side nav's recent sessions narrow on their own */
  recentFilter: SessionFilter;

  sessionNodes: string[] | null;
  mailboxNodes: string[];
  mailboxKinds: string[];
};

const initial = (): ListViews => ({
  sessionFilter: emptyFilter,
  recentFilter: emptyFilter,
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
    recentFilter: restoreSessionFilter(saved.recentFilter),
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

export const listViews = {
  setSessionFilter(sessionFilter: SessionFilter) {
    store.setState({ sessionFilter });
  },

  setRecentFilter(recentFilter: SessionFilter) {
    store.setState({ recentFilter });
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

export function useListViews<T>(select: (views: ListViews) => T): T {
  return useStore(store, select);
}

export const resetListViews = () => store.setState(initial(), true);
