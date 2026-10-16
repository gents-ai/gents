import { useCallback, type SetStateAction } from "react";
import { useStore } from "zustand";
import { createStore, type StoreApi } from "zustand/vanilla";

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
