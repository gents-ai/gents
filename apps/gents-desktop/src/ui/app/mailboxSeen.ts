import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";
import { useStore } from "zustand";
import type { MailboxItemView } from "@source-inc/gents-desktop-client";

import { browserStorage } from "@/lib/storage";

/** The mailbox items opened on this device, most recent first, and when it
    started keeping them. An item is new until it is opened here; one that
    arrived before `since` never counts as new. */
type MailboxSeen = { opened: string[]; since: string | null };

const KEPT = 500;

const initial = (): MailboxSeen => ({ opened: [], since: null });

/* item ids are a node's own, so a key carries its node */
const keyOf = (item: Pick<MailboxItemView, "nodeDid" | "itemId">) =>
  `${item.nodeDid}:${item.itemId}`;

export function restoreMailboxSeen(stored: unknown): MailboxSeen {
  const saved = (typeof stored === "object" && stored !== null ? stored : {}) as Record<
    string,
    unknown
  >;
  const opened = Array.isArray(saved.opened) ? saved.opened : [];
  return {
    opened: opened
      .filter((key): key is string => typeof key === "string")
      .slice(0, KEPT),
    since: typeof saved.since === "string" ? saved.since : null,
  };
}

const store = createStore<MailboxSeen>()(
  persist(initial, {
    name: "gents-mailbox-seen",
    version: 1,
    storage: createJSONStorage(() => browserStorage),
    merge: (stored) => restoreMailboxSeen(stored),
  }),
);

export function isNewItem(
  item: Pick<MailboxItemView, "nodeDid" | "itemId" | "createdAt">,
  seen: MailboxSeen,
): boolean {
  if (seen.since === null || seen.opened.includes(keyOf(item))) return false;
  return Date.parse(item.createdAt) > Date.parse(seen.since);
}

export const useNewItem = (item: MailboxItemView) =>
  useStore(store, (state) => isNewItem(item, state));

export const mailboxSeen = {
  open(item: Pick<MailboxItemView, "nodeDid" | "itemId">) {
    const key = keyOf(item);
    const { opened } = store.getState();
    if (opened[0] === key) return;
    store.setState({
      opened: [key, ...opened.filter((other) => other !== key)].slice(0, KEPT),
    });
  },
  /** the first call on a device sets when it started keeping them */
  start(now: string) {
    if (!store.getState().since) store.setState({ since: now });
  },
};

export const resetMailboxSeen = () => store.setState(initial(), true);
