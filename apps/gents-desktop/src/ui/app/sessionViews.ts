import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import { browserStorage } from "@/lib/storage";

/** Where the reader left a session: the row read, its distance below the
    top of the view, and its message number, for reading the page around it
    when it is not loaded. */
export type ReadingPlace = { key: string; offset: number; sequence: number | null };

/** How each session was last seen on this device, for the sessions seen
    most recently. It is the reader's, not the session's: nothing here is
    session data. */
export type SessionViews = {
  /** by session, the most recently read first */
  places: { session: string; place: ReadingPlace }[];
};

export const PLACES_KEPT = 200;

const initial = (): SessionViews => ({ places: [] });

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
  return {
    places: places
      .filter(
        (entry): entry is SessionViews["places"][number] =>
          typeof entry?.session === "string" && isPlace(entry.place),
      )
      .slice(0, PLACES_KEPT),
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
};

export const resetSessionViews = () => store.setState(initial(), true);
