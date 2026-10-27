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

  mailboxKinds: string[];
};

const initial = (): ListViews => ({
  sessionFilter: emptyFilter,
  recentFilter: emptyFilter,
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
  setMailboxKinds(mailboxKinds: string[]) {
    store.setState({ mailboxKinds });
  },
};

export function useListViews<T>(select: (views: ListViews) => T): T {
  return useStore(store, select);
}

export const resetListViews = () => store.setState(initial(), true);
