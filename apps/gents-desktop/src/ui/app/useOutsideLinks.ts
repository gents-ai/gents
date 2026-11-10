import { useEffect } from "react";

import {
  inNativeShell,
  listenToWindow,
  OPEN_LINK_EVENT,
  takeOpenedLinks,
  windowLabel,
} from "../../lib/nativeShell";
import { openLink } from "../lib/router";

/** Where links from outside the app come from: the ones waiting, and word
    that more arrived. */
export type LinkSource = {
  take: () => Promise<string[]>;
  onArrived: (arrived: () => void) => Promise<() => void>;
};

let native: LinkSource | null | undefined;
const nativeLinks = (): LinkSource | null =>
  (native ??=
    inNativeShell() && windowLabel() === "main"
      ? {
          take: takeOpenedLinks,
          onArrived: (arrived) => listenToWindow(OPEN_LINK_EVENT, arrived),
        }
      : null);

/** Follows the `gents:` links the system opens the app with (the one it was
    launched with, and each one after), once `ready`: the shell holds them
    until then, so a link opened during startup or setup isn't lost. */
export function useOutsideLinks(
  ready: boolean,
  source: LinkSource | null = nativeLinks(),
  open: (href: string) => boolean = openLink,
) {
  useEffect(() => {
    if (!ready || !source) return;
    const take = () =>
      void source
        .take()
        .then((links) => links.forEach((link) => open(link)))
        .catch((error) => console.warn("could not take the links opened", error));
    const stop = source.onArrived(take);
    take();
    return () => void stop.then((unlisten) => unlisten());
  }, [ready, source, open]);
}
