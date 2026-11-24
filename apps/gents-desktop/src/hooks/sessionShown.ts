import { createStore, type StoreApi } from "zustand/vanilla";

/** Whether the selected session is on screen. A narrow window shows one
    page at a time, so going back from a session to the list leaves it
    selected but out of sight: work it finishes then is not seen. */
export type SessionShown = { shown: boolean };

export type SessionShownStore = StoreApi<SessionShown>;

export const createSessionShownStore = (): SessionShownStore =>
  createStore<SessionShown>(() => ({ shown: true }));

/** A wide window keeps the selected session beside whatever page shows; a
    narrow one shows it only on its own page (Lean `SessionShown.shown`). */
export const sessionShownOn = (wide: boolean, routeName: string): boolean =>
  wide || routeName === "session";

/** Whether the selected session's seen mark is kept now: the window is
    visible, the session is on screen and the list holds it (Lean
    `SessionShown.keepsSeen`). */
export const keepsSeen = (visible: boolean, shown: boolean, listed: boolean): boolean =>
  visible && shown && listed;
