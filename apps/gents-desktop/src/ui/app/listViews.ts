import { useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import {
  emptyFilter,
  restoreSessionFilter,
  type SessionFilter,
} from "@/lib/session-filter";
import { browserStorage } from "@/lib/storage";

/** The mailbox's tabs: all items, or one triage group. */
export type MailboxTab = "all" | "decide" | "wrong" | "look" | "done";
const MAILBOX_TABS: readonly MailboxTab[] = ["all", "decide", "wrong", "look", "done"];

/** The item open in the mailbox's dialog, on the node it belongs to; its
    place in the tab's list, for the one after it if it has gone when the
    person comes back; and the page it was opened over, the only one it
    shows on, so it waits while the person looks at a session from it. */
export type MailboxOpen = {
  nodeDid: string | null;
  itemId: string;
  at: number;
  where: string;
};

/** The side nav's recent sessions leave out sessions another session started:
    they belong to the work that started them, which shows them, and a few
    workers would fill the list. */
export const recentDefault: SessionFilter = {
  ...emptyFilter,
  sources: ["person", "task"],
};

export type ListViews = {
  sessionFilter: SessionFilter;
  /* the side nav's recent sessions narrow on their own */
  recentFilter: SessionFilter;

  mailboxTab: MailboxTab;
  /* kept while the person looks at a session and comes back; not across a
     restart, which would open a dialog unasked */
  mailboxOpen: MailboxOpen | null;
  /* the mailbox's search, null when closed; the dialog steps through what
     it leaves. Not kept across a restart. */
  mailboxQuery: string | null;
};

const initial = (): ListViews => ({
  sessionFilter: emptyFilter,
  recentFilter: recentDefault,
  mailboxTab: "all",
  mailboxOpen: null,
  mailboxQuery: null,
});

export function restoreListViews(stored: unknown): ListViews {
  const saved = (typeof stored === "object" && stored !== null ? stored : {}) as Record<
    string,
    unknown
  >;
  return {
    sessionFilter: restoreSessionFilter(saved.sessionFilter),
    recentFilter:
      saved.recentFilter === undefined
        ? recentDefault
        : restoreSessionFilter(saved.recentFilter),
    mailboxTab: MAILBOX_TABS.find((tab) => tab === saved.mailboxTab) ?? "all",
    mailboxOpen: null,
    mailboxQuery: null,
  };
}

/** Version 2 leaves workers out of the recent sessions by default; a recent
    filter saved earlier that chose nothing about who started a session takes
    that default, and one that did is kept. */
export function migrateListViews(stored: unknown, version: number): unknown {
  if (version >= 2 || typeof stored !== "object" || stored === null) return stored;
  const saved = stored as Record<string, unknown>;
  const recent = restoreSessionFilter(saved.recentFilter);
  return recent.sources.length > 0
    ? saved
    : { ...saved, recentFilter: { ...recent, sources: recentDefault.sources } };
}

const store = createStore<ListViews>()(
  persist(initial, {
    name: "gents-list-views",
    version: 2,
    storage: createJSONStorage(() => browserStorage),
    migrate: migrateListViews,
    merge: (stored) => restoreListViews(stored),
    partialize: ({ mailboxOpen: _open, mailboxQuery: _query, ...kept }) => kept,
  }),
);

export const listViews = {
  setSessionFilter(sessionFilter: SessionFilter) {
    store.setState({ sessionFilter });
  },

  setRecentFilter(recentFilter: SessionFilter) {
    store.setState({ recentFilter });
  },
  setMailboxTab(mailboxTab: MailboxTab) {
    store.setState({ mailboxTab });
  },
  setMailboxOpen(mailboxOpen: MailboxOpen | null) {
    store.setState({ mailboxOpen });
  },
  setMailboxQuery(mailboxQuery: string | null) {
    store.setState({ mailboxQuery });
  },
};

export function useListViews<T>(select: (views: ListViews) => T): T {
  return useStore(store, select);
}

export const resetListViews = () => store.setState(initial(), true);
