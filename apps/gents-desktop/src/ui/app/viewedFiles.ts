import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";
import { useStore } from "zustand";

import { browserStorage } from "@/lib/storage";
import type { ChangedFile } from "@/lib/turnChanges";
import type { SessionScope } from "../../lib/recordedFiles";

/** A changed file marked viewed on this device, with the edit it had then:
    a later edit is a version not yet viewed. */
type Viewed = { file: string; edit: string };

type ViewedFiles = { viewed: Viewed[] };

export const VIEWED_KEPT = 500;

const keyOf = (scope: SessionScope, path: string) =>
  [scope.nodeDid, scope.sessionId, path].join("\0");

/* a file's version in a turn: its last edit there */
const versionOf = (file: ChangedFile) =>
  file.edits[file.edits.length - 1]?.toolCallDocId ?? null;

const isViewed = (state: ViewedFiles, scope: SessionScope, file: ChangedFile) => {
  const edit = versionOf(file);
  const key = keyOf(scope, file.path);
  return edit !== null && state.viewed.some((v) => v.file === key && v.edit === edit);
};

function restore(stored: unknown): ViewedFiles {
  const viewed = (stored as { viewed?: unknown } | null)?.viewed;
  return {
    viewed: (Array.isArray(viewed) ? viewed : [])
      .filter(
        (entry): entry is Viewed =>
          typeof entry?.file === "string" && typeof entry?.edit === "string",
      )
      .slice(0, VIEWED_KEPT),
  };
}

const store = createStore<ViewedFiles>()(
  persist(() => ({ viewed: [] }), {
    name: "gents-viewed-files",
    version: 1,
    storage: createJSONStorage(() => browserStorage),
    merge: (stored) => restore(stored),
  }),
);

/** Whether `file`, as this turn changed it, was marked viewed here. */
export const useViewed = (scope: SessionScope, file: ChangedFile): boolean =>
  useStore(store, (state) => isViewed(state, scope, file));

export const viewedFiles = {
  isViewed: (scope: SessionScope, file: ChangedFile) =>
    isViewed(store.getState(), scope, file),
  /** marks the file's current version viewed, or not; the most recent first */
  set(scope: SessionScope, file: ChangedFile, viewed: boolean) {
    const key = keyOf(scope, file.path);
    const edit = versionOf(file);
    const others = store
      .getState()
      .viewed.filter((v) => v.file !== key || v.edit !== edit);
    store.setState({
      viewed:
        viewed && edit !== null
          ? [{ file: key, edit }, ...others].slice(0, VIEWED_KEPT)
          : others,
    });
  },
};

export const resetViewedFiles = () => store.setState({ viewed: [] });
