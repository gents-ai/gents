/* This computer's home folder, read once from the shell on first use. A
   path from another node's host must not be shown against it. */
import { useSyncExternalStore } from "react";
import { homeFolder } from "../../lib/nativeShell";

let home: string | null = null;
let requested = false;
const listeners = new Set<() => void>();

function subscribe(listener: () => void) {
  listeners.add(listener);
  if (!requested) {
    requested = true;
    homeFolder()
      .then((found) => {
        home = found;
        for (const notify of listeners) notify();
      })
      .catch(() => undefined);
  }
  return () => listeners.delete(listener);
}

export function useHomeFolder(): string | null {
  return useSyncExternalStore(
    subscribe,
    () => home,
    () => null,
  );
}
