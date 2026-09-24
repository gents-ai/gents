/* a media query as React state, without a setState in an effect */
import { useSyncExternalStore } from "react";

/* Below the width the full desktop layout was built for, the expanded nav and
   the docked side panel give way to overlays so the canvas keeps its room. */
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
