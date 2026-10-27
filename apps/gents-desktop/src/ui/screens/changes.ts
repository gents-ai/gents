import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import { preferFile } from "../../lib/fileReads";
import type { SessionScope } from "../../lib/recordedFiles";
import type { ChangedFile } from "@/lib/turnChanges";

/** The turn the Changes panel shows, and the file chosen in it. */
export type ShownChanges = {
  scope: SessionScope;
  turn: string;
  files: readonly ChangedFile[];
  path: string;
};

const store = createStore<Record<string, ShownChanges>>(() => ({}));

export const changesView = {
  show(shown: ShownChanges) {
    preferFile(shown.scope, shown.path);
    store.setState({ [shown.scope.sessionId]: shown });
  },
  /** choosing the chosen file again brings it back to the top and opens it */
  choose(sessionId: string, path: string) {
    const shown = store.getState()[sessionId];
    if (!shown) return;
    preferFile(shown.scope, path);
    store.setState({ [sessionId]: { ...shown, path } });
  },
};

/* The panel's file list stays as the reader left it, across turns; its
   width, dragged, on this device. */
const WIDTH_KEY = "gents.changes.fileListWidth";
const savedWidth = () => {
  try {
    const width = Number(localStorage.getItem(WIDTH_KEY));
    return width > 0 ? width : null;
  } catch {
    return null;
  }
};
const layout = createStore(() => ({
  fileList: true,
  width: savedWidth() as number | null,
}));

export const toggleFileList = () => layout.setState((s) => ({ fileList: !s.fileList }));

export const useFileListOpen = () => useStore(layout, (s) => s.fileList);

export const useFileListWidth = () => useStore(layout, (s) => s.width);

/** null returns the list to its own width */
export const setFileListWidth = (width: number | null) => layout.setState({ width });

/** Keeps the list's width for the next time, once a drag or step ends. */
export function keepFileListWidth() {
  const { width } = layout.getState();
  try {
    if (width === null) localStorage.removeItem(WIDTH_KEY);
    else localStorage.setItem(WIDTH_KEY, String(Math.round(width)));
  } catch {}
}

export const useShownChanges = (sessionId: string | null) =>
  useStore(store, (s) => (sessionId ? s[sessionId] : undefined));

/** The file chosen in the panel when it shows `turn`, else null: a card
    follows only its own turn, so choosing a file redraws no other card. */
export const useChosenPath = (sessionId: string, turn: string) =>
  useStore(store, (s) => (s[sessionId]?.turn === turn ? s[sessionId].path : null));

export const resetChangesView = () => {
  store.setState({}, true);
  layout.setState({ fileList: true, width: null });
};
