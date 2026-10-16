import { createElement, Fragment, useSyncExternalStore } from "react";

export const when = (iso: string | null, now: number) => {
  if (!iso) return "";
  const mins = Math.round((now - Date.parse(iso)) / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  if (mins < 1_440) return `${Math.round(mins / 60)}h`;
  if (mins < 7 * 1_440) return `${Math.round(mins / 1_440)}d`;
  return new Date(iso).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
};

export const span = (ms: number) => {
  const mins = Math.max(1, Math.round(ms / 60_000));
  if (mins < 60) return `${mins}m`;
  if (mins < 1_440) return `${Math.round(mins / 60)}h`;
  return `${Math.round(mins / 1_440)}d`;
};

const listeners = new Set<() => void>();
let timer: number | null = null;
let minute = Math.floor(Date.now() / 60_000);
function subscribe(listener: () => void) {
  listeners.add(listener);
  if (timer === null) {
    timer = window.setInterval(() => {
      const next = Math.floor(Date.now() / 60_000);
      if (next === minute) return;
      minute = next;
      for (const notify of [...listeners]) notify();
    }, 15_000);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && timer !== null) {
      window.clearInterval(timer);
      timer = null;
    }
  };
}
const currentMinute = () => {
  if (timer === null) minute = Math.floor(Date.now() / 60_000);
  return minute * 60_000;
};

export function useMinute(): number {
  return useSyncExternalStore(subscribe, currentMinute, currentMinute);
}

export function Age({ iso }: { iso: string | null }) {
  return createElement(Fragment, null, when(iso, useMinute()));
}
