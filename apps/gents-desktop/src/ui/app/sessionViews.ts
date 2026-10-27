import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";
import { useStore } from "zustand";

import { browserStorage } from "@/lib/storage";
import type { SessionKey } from "../../hooks/fleetStore";

/** Where the reader left a session: the row read, its distance below the
    top of the view, and its message number, for reading the page around it
    when it is not loaded. */
export type ReadingPlace = { key: string; offset: number; sequence: number | null };

/** What a session had done when it was last on screen: its latest request,
    and whether that request had finished. */
export type SeenMark = { request: string | null; done: boolean };

/** How each session was last seen on this device, for the sessions seen
    most recently. It is the reader's, not the session's: nothing here is
    session data. */
export type SessionViews = {
  /** by session, the most recently read first */
  places: { session: string; place: ReadingPlace }[];
  /** by session, the started sessions its dock shows nested, the most
      recently changed first */
  opened: { session: string; rows: string[] }[];
  /** by node-qualified session key, the most recently seen first */
  seen: { session: SessionKey; mark: SeenMark }[];
  /** when this device started keeping `seen`: work that ended before it,
      in a session never seen here, is not new */
  since: string | null;
};

export const PLACES_KEPT = 200;
export const SEEN_KEPT = 500;

const initial = (): SessionViews => ({ places: [], opened: [], seen: [], since: null });

const isSeenMark = (value: unknown): value is SeenMark => {
  if (typeof value !== "object" || value === null) return false;
  const mark = value as Record<string, unknown>;
  return (
    (mark.request === null || typeof mark.request === "string") &&
    typeof mark.done === "boolean"
  );
};

function isPlace(value: unknown): value is ReadingPlace {
  if (typeof value !== "object" || value === null) return false;
  const place = value as Record<string, unknown>;
  return (
    typeof place.key === "string" &&
    typeof place.offset === "number" &&
    Number.isFinite(place.offset) &&
    (place.sequence === null ||
      (typeof place.sequence === "number" && Number.isInteger(place.sequence)))
  );
}

export function restoreSessionViews(stored: unknown): SessionViews {
  const saved = (typeof stored === "object" && stored !== null ? stored : {}) as Record<
    string,
    unknown
  >;
  const places = Array.isArray(saved.places) ? saved.places : [];
  const opened = Array.isArray(saved.opened) ? saved.opened : [];
  const seen = Array.isArray(saved.seen) ? saved.seen : [];
  return {
    places: places
      .filter(
        (entry): entry is SessionViews["places"][number] =>
          typeof entry?.session === "string" && isPlace(entry.place),
      )
      .slice(0, PLACES_KEPT),
    opened: opened
      .filter(
        (entry): entry is SessionViews["opened"][number] =>
          typeof entry?.session === "string" &&
          Array.isArray(entry.rows) &&
          entry.rows.every((row: unknown) => typeof row === "string"),
      )
      .slice(0, PLACES_KEPT),
    seen: seen
      .filter(
        (entry): entry is SessionViews["seen"][number] =>
          typeof entry?.session === "string" && isSeenMark(entry.mark),
      )
      .slice(0, SEEN_KEPT),
    since: typeof saved.since === "string" ? saved.since : null,
  };
}

const store = createStore<SessionViews>()(
  persist(initial, {
    name: "gents-session-views",
    version: 1,
    storage: createJSONStorage(() => browserStorage),
    merge: (stored) => restoreSessionViews(stored),
  }),
);

export const sessionViews = {
  placeOf(session: string | null): ReadingPlace | null {
    if (!session) return null;
    return (
      store.getState().places.find((entry) => entry.session === session)?.place ?? null
    );
  },
  /** a place saved moves its session to the front; none, at the foot,
      forgets it */
  setPlace(session: string, place: ReadingPlace | null) {
    const others = store.getState().places.filter((entry) => entry.session !== session);
    store.setState({
      places: (place ? [{ session, place }, ...others] : others).slice(0, PLACES_KEPT),
    });
  },
  openedOf(session: string | null): readonly string[] {
    if (!session) return [];
    return (
      store.getState().opened.find((entry) => entry.session === session)?.rows ?? []
    );
  },
  /** none open forgets the session */
  setOpened(session: string, rows: readonly string[]) {
    const others = store.getState().opened.filter((entry) => entry.session !== session);
    store.setState({
      opened: (rows.length ? [{ session, rows: [...rows] }, ...others] : others).slice(
        0,
        PLACES_KEPT,
      ),
    });
  },
};

const seenOf = (state: SessionViews, session: SessionKey) =>
  state.seen.find((entry) => entry.session === session)?.mark ?? null;

/** What `session` had done when it was last on screen here, kept current. */
export const useSeenMark = (session: SessionKey): SeenMark | null =>
  useStore(store, (state) => seenOf(state, session));

/** When this device started keeping what was seen, kept current. */
export const useSeenSince = (): string | null =>
  useStore(store, (state) => state.since);

export const seenMarks = {
  of: (session: SessionKey) => seenOf(store.getState(), session),
  /** a mark saved moves its session to the front; one unchanged writes nothing */
  set(session: SessionKey, mark: SeenMark) {
    const state = store.getState();
    const was = seenOf(state, session);
    if (was && was.request === mark.request && was.done === mark.done) return;
    const others = state.seen.filter((entry) => entry.session !== session);
    store.setState({ seen: [{ session, mark }, ...others].slice(0, SEEN_KEPT) });
  },
  /** the first call on a device sets when it started keeping marks */
  start(now: string) {
    if (!store.getState().since) store.setState({ since: now });
  },
};

export const resetSessionViews = () => store.setState(initial(), true);
