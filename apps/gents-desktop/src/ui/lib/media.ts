/* a media query as React state, without a setState in an effect */
import { useSyncExternalStore } from "react";

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
