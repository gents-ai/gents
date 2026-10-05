import type { SetStateAction } from "react";
import { createStore, type StoreApi } from "zustand/vanilla";

import type {
  ChatWorkflowState,
  OptimisticPendingTurn,
} from "@source-inc/gents-desktop-chat";

import { loadChatFolders, type ChatFolders } from "./chatFolders";
import { createSelectors, type WithSelectors } from "./createSelectors";

/** The compose side of a chat: the submission workflow the client runs
    locally, whether a send is in flight, the turn shown for a sent
    message until the transcript holds it, and each chat's folder. */
export type ChatState = {
  localWorkflow: ChatWorkflowState;
  sending: boolean;
  optimisticPendingTurn: OptimisticPendingTurn | null;
  folders: ChatFolders;
};

export type ChatStore = WithSelectors<StoreApi<ChatState>>;

export function createChatStore(initial: Partial<ChatState> = {}) {
  return createSelectors(
    createStore<ChatState>(() => ({
      localWorkflow: { kind: "ready" },
      sending: false,
      optimisticPendingTurn: null,
      folders: loadChatFolders(),
      ...initial,
    })),
  );
}

/** A setter for one field, taking a value or an updater, as React's did. */
export function setterOf<K extends keyof ChatState>(store: ChatStore, key: K) {
  return (next: SetStateAction<ChatState[K]>) =>
    store.setState((state) => {
      const value =
        typeof next === "function"
          ? (next as (current: ChatState[K]) => ChatState[K])(state[key])
          : next;
      return Object.is(value, state[key]) ? state : { [key]: value };
    });
}
