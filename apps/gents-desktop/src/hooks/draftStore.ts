import { useCallback, type SetStateAction } from "react";
import { useStore } from "zustand";
import { createStore, type StoreApi } from "zustand/vanilla";

/**
 * Composer drafts by context (a session, or the new-session screen for an
 * agent and behavior). Held outside React state so a keystroke reaches the
 * composer that shows the draft rather than the whole shell.
 */
export type DraftStore = StoreApi<{ drafts: Record<string, string> }>;

export function createDraftStore(): DraftStore {
  return createStore(() => ({ drafts: {} }));
}

export function writeDraft(
  store: DraftStore,
  key: string,
  next: SetStateAction<string>,
) {
  store.setState((state) => {
    const current = state.drafts[key] ?? "";
    const value = typeof next === "function" ? next(current) : next;
    if (value === current) return state;
    const drafts = { ...state.drafts };
    if (value) drafts[key] = value;
    else delete drafts[key];
    return { drafts };
  });
}

/** The draft for `key` and its setter; re-renders only when that draft changes. */
export function useDraft(
  store: DraftStore,
  key: string,
): [string, (next: SetStateAction<string>) => void] {
  const draft = useStore(store, (state) => state.drafts[key] ?? "");
  const setDraft = useCallback(
    (next: SetStateAction<string>) => writeDraft(store, key, next),
    [store, key],
  );
  return [draft, setDraft];
}
