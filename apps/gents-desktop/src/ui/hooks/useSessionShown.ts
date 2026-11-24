import { useEffect } from "react";

import { useApp } from "../app/AppContext";
import { useMediaQuery } from "../lib/media";
import type { Route } from "../lib/router";
import { sessionShownOn } from "../../hooks/sessionShown";

/** Tells the app whether the selected session is on screen at `route`. */
export function useSessionShown(route: Route) {
  const { sessionShown } = useApp();
  const shown = sessionShownOn(useMediaQuery("(min-width: 768px)"), route.name);
  useEffect(() => {
    sessionShown.setState({ shown });
  }, [sessionShown, shown]);
}
