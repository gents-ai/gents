import { useSyncExternalStore } from "react";

export const ROOMY_WINDOW = "(min-width: 1180px)";

export function useMediaQuery(query: string) {
  return useSyncExternalStore(
    (notify) => {
      const list = matchMedia(query);
      list.addEventListener("change", notify);
      return () => list.removeEventListener("change", notify);
    },
    () => matchMedia(query).matches,
    () => true,
  );
}
