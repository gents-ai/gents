import { useEffect, useState } from "react";

import type { SharingState } from "../../hooks/sharing";
import { useApp } from "../app/AppContext";

export type SharingView =
  | { status: "loading" }
  /* the node's runtime can't share yet */
  | { status: "unavailable" }
  | { status: "ready"; state: SharingState }
  | { status: "error"; message: string };

/** a node's sharing, read again whenever the adapter says it changed */
export function useSharing(nodeDid: string | null): SharingView {
  const { sharing } = useApp();
  const [view, setView] = useState<SharingView>({ status: "loading" });
  useEffect(() => {
    if (!nodeDid) {
      setView({ status: "unavailable" });
      return;
    }
    let live = true;
    /* reads can overlap when changes come quickly; only the latest lands */
    let latest = 0;
    const load = () => {
      const read = ++latest;
      sharing.read(nodeDid).then(
        (state) => {
          if (live && read === latest)
            setView(state ? { status: "ready", state } : { status: "unavailable" });
        },
        (error: unknown) => {
          if (live && read === latest)
            setView({
              status: "error",
              message: error instanceof Error ? error.message : String(error),
            });
        },
      );
    };
    setView({ status: "loading" });
    load();
    const stop = sharing.subscribe(nodeDid, load);
    return () => {
      live = false;
      stop();
    };
  }, [sharing, nodeDid]);
  return view;
}

/** the adapter's actions, for buttons */
export function useSharingActions() {
  return useApp().sharing;
}
