import { createStore } from "zustand/vanilla";
import { useStore } from "zustand";

import { navigate } from "@/lib/router";

/** A place in a session that anything can send the app to: a message, by
    its number, and optionally the row to show there (a compaction marker, a
    reply) and whether to open it. The transcript reads the page around the
    message when it isn't loaded, so nothing reads the history before it. */
export type MessagePlace = {
  sessionId: string;
  nodeDid?: string | null;
  sequence: number;
  itemKey?: string | null;
  open?: boolean;
};

/* each request is new, so asking for the same place twice goes there twice */
type PlaceRequest = MessagePlace & { id: number };

type State = { request: PlaceRequest | null; opening: string | null };

const store = createStore<State>()(() => ({ request: null, opening: null }));
let next = 0;

export function openAt(place: MessagePlace) {
  next += 1;
  store.setState({ request: { ...place, id: next } });
  navigate({
    name: "session",
    sessionId: place.sessionId,
    ...(place.nodeDid ? { nodeDid: place.nodeDid } : {}),
  });
}

export const messagePlaces = {
  request: () => store.getState().request,
  /* the transcript has settled the request; a row asked to open opens
     only when it was the row landed on, not a fallback to its message */
  landed(id: number, onRow: boolean) {
    const request = store.getState().request;
    if (request?.id !== id) return;
    store.setState({
      request: null,
      opening: onRow && request.open && request.itemKey ? request.itemKey : null,
    });
  },
  opened(itemKey: string) {
    if (store.getState().opening === itemKey) store.setState({ opening: null });
  },
};

export function usePlaceRequest(sessionId: string | null) {
  return useStore(store, (state) =>
    sessionId && state.request?.sessionId === sessionId ? state.request : null,
  );
}

export function useOpening(itemKey: string) {
  return useStore(store, (state) => state.opening === itemKey);
}
