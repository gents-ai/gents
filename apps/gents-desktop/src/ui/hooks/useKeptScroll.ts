import { useLayoutEffect, useRef } from "react";

/** where a list keeps its scroll, by the key naming the list */
export type ScrollPlace<K extends string> = {
  read: (key: K) => number;
  keep: (key: K, top: number) => void;
};

/** a place held in memory only: nothing outlives the app */
export function scrollPlaceInMemory(): ScrollPlace<string> {
  const tops = new Map<string, number>();
  return {
    read: (key) => tops.get(key) ?? 0,
    keep: (key, top) => void tops.set(key, top),
  };
}

/* A list is put back where it was scrolled to once its rows are drawn, and
   every scroll after is kept; a new key is put back to its own place. Until
   the rows are drawn no scroll counts, and listening in a layout effect stops
   before the list's rows are taken away, so the scroll that clamps doesn't
   count either. */
export function useKeptScroll<K extends string>(
  scroller: HTMLElement | null,
  key: K,
  drawn: boolean,
  place: ScrollPlace<K>,
) {
  const pending = useRef<number | null>(null);
  const shown = useRef<K | null>(null);
  useLayoutEffect(() => {
    if (shown.current !== key) {
      shown.current = key;
      pending.current = place.read(key);
    }
    if (!scroller || !drawn || pending.current === null) return;
    scroller.scrollTop = pending.current;
    pending.current = null;
  }, [scroller, key, drawn, place]);
  useLayoutEffect(() => {
    if (!scroller) return;
    const keep = () => {
      if (pending.current === null) place.keep(key, scroller.scrollTop);
    };
    scroller.addEventListener("scroll", keep, { passive: true });
    return () => scroller.removeEventListener("scroll", keep);
  }, [scroller, key, place]);
}
