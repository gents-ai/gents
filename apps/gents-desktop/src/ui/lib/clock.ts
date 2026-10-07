/* the current time as React state, ticking once a second only while
   something on screen is still running */
import { useSyncExternalStore } from "react";

let now = Date.now();
const listeners = new Set<() => void>();
let timer: number | null = null;
const start = () => {
  if (timer !== null) return;
  /* the clock stood still while nothing ran; the first reader is told the
     time now, not when it stopped */
  now = Date.now();
  timer = window.setInterval(() => {
    now = Date.now();
    for (const l of listeners) l();
  }, 1000);
};
const stop = () => {
  if (timer !== null && listeners.size === 0) {
    window.clearInterval(timer);
    timer = null;
  }
};

/* stable across renders: a new subscribe each render would resubscribe,
   restart the clock and rewrite `now`, rendering again without end */
const subscribe = (notify: () => void) => {
  listeners.add(notify);
  start();
  return () => {
    listeners.delete(notify);
    stop();
  };
};
const idle = () => () => undefined;

export function useNow(active: boolean) {
  return useSyncExternalStore(
    active ? subscribe : idle,
    () => (active ? now : 0),
    () => 0,
  );
}
