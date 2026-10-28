import { useCallback, type SetStateAction } from "react";
import { useStore } from "zustand";
import { createStore, type StoreApi } from "zustand/vanilla";

export type DraftAttachment = {
  id: string;
  name: string;
  content?: import("@source-inc/gents-desktop-client").RequestAttachment;
  preview?: string;
  error?: string;
};
export type DraftStore = StoreApi<{
  drafts: Record<string, string>;
  attachments: Record<string, DraftAttachment[]>;
  /** each draft's rich field as the composer last saved it, so its chips
      come back; the text in `drafts` stays what is sent */
  fields: Record<string, string>;
  /** the draft whose composer takes focus once it next accepts input */
  focus: string | null;
}>;

export function createDraftStore(): DraftStore {
  return createStore(() => ({ drafts: {}, attachments: {}, fields: {}, focus: null }));
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
    if (value || !(key in state.fields)) return { drafts };
    const fields = { ...state.fields };
    delete fields[key];
    return { drafts, fields };
  });
}

/** the composer's saved field for a draft; an emptied draft drops it */
export function useDraftField(
  store: DraftStore,
  key: string,
): [string | null, (field: string) => void] {
  const field = useStore(store, (state) => state.fields[key] ?? null);
  const setField = useCallback(
    (next: string) =>
      store.setState((state) =>
        !state.drafts[key] || state.fields[key] === next
          ? state
          : { fields: { ...state.fields, [key]: next } },
      ),
    [store, key],
  );
  return [field, setField];
}

export function focusDraft(store: DraftStore, key: string | null) {
  if (store.getState().focus !== key) store.setState({ focus: key });
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

export function writeAttachments(
  store: DraftStore,
  key: string,
  update: (items: DraftAttachment[]) => DraftAttachment[],
) {
  store.setState((state) => {
    const before = state.attachments[key] ?? [];
    const after = update(before);
    for (const item of before)
      if (item.preview && !after.some((a) => a.preview === item.preview))
        URL.revokeObjectURL(item.preview);
    return { attachments: { ...state.attachments, [key]: after } };
  });
}
