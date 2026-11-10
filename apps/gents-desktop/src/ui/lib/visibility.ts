import { useSyncExternalStore } from "react";

const subscribe = (notify: () => void) => {
  document.addEventListener("visibilitychange", notify);
  return () => document.removeEventListener("visibilitychange", notify);
};
const shown = () => document.visibilityState !== "hidden";

/** Whether the window shows the page, as the web view reports it. */
export function usePageShown(): boolean {
  return useSyncExternalStore(subscribe, shown, () => true);
}
