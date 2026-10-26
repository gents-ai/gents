import { useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import { browserStorage } from "./lib/storage";
import { applyTheme, systemTheme, type ThemePreference } from "./theme";
import { ZOOM_STEPS } from "./lib/zoom";

export type NavMode = "hover" | "expanded" | "collapsed";

/** Where a message the person sends settles: the view keeps following the
    foot as the reply grows, or the message is kept near the top of the view
    with the reply filling in beneath it. */
export type SentTurnPlacement = "follow" | "anchor";

/** What a message sent while a turn runs does: it is folded into that turn
    at its next safe step, or it waits for the turn to finish. */
export type FollowUp = "steer" | "queue";

export type Preferences = {
  theme: ThemePreference;
  nav: NavMode;
  zoom: number;
  sentTurn: SentTurnPlacement;
  followUp: FollowUp;
};

const NAV_MODES: readonly string[] = ["hover", "expanded", "collapsed"];

const initial = (): Preferences => ({
  theme: systemTheme(),
  nav: "hover",
  zoom: 1,
  sentTurn: "anchor",
  followUp: "steer",
});

export function restorePreferences(
  stored: unknown,
  defaults: Preferences,
): Preferences {
  const saved = (typeof stored === "object" && stored !== null ? stored : {}) as Record<
    string,
    unknown
  >;
  return {
    theme:
      saved.theme === "light" || saved.theme === "dark" ? saved.theme : defaults.theme,
    nav:
      typeof saved.nav === "string" && NAV_MODES.includes(saved.nav)
        ? (saved.nav as NavMode)
        : defaults.nav,
    zoom: (ZOOM_STEPS as readonly unknown[]).includes(saved.zoom)
      ? (saved.zoom as number)
      : defaults.zoom,
    sentTurn:
      saved.sentTurn === "follow" || saved.sentTurn === "anchor"
        ? saved.sentTurn
        : defaults.sentTurn,
    followUp:
      saved.followUp === "steer" || saved.followUp === "queue"
        ? saved.followUp
        : defaults.followUp,
  };
}

const store = createStore<Preferences>()(
  persist(initial, {
    name: "gents-preferences",
    version: 1,
    storage: createJSONStorage(() => browserStorage),
    merge: (stored, current) => restorePreferences(stored, current),
  }),
);

export const preferences = {
  setTheme(theme: ThemePreference) {
    store.setState({ theme });
    applyTheme(theme);
  },
  setNav(nav: NavMode) {
    store.setState({ nav });
  },
  setZoom(zoom: number) {
    store.setState({ zoom });
  },
  setSentTurn(sentTurn: SentTurnPlacement) {
    store.setState({ sentTurn });
  },
  setFollowUp(followUp: FollowUp) {
    store.setState({ followUp });
  },
};

export function followZoom(apply: (zoom: number) => void) {
  apply(store.getState().zoom);
  return store.subscribe((next, prev) => {
    if (next.zoom !== prev.zoom) apply(next.zoom);
  });
}

/** Browser storage events notify other windows, which hold independent stores. */
export function followOtherWindows() {
  const reread = (event: StorageEvent) => {
    if (event.key !== "gents-preferences") return;
    void Promise.resolve(store.persist.rehydrate()).then(applyChosenTheme);
  };
  window.addEventListener("storage", reread);
  return () => window.removeEventListener("storage", reread);
}

export const applyChosenTheme = () => applyTheme(store.getState().theme);

export function usePreferences<T>(select: (preferences: Preferences) => T): T {
  return useStore(store, select);
}

export const useTheme = () => usePreferences((p) => p.theme);
export const useNavMode = () => usePreferences((p) => p.nav);
export const zoomLevel = () => store.getState().zoom;
export const useSentTurnPlacement = () => usePreferences((p) => p.sentTurn);
export const useFollowUp = () => usePreferences((p) => p.followUp);

export const resetPreferences = () => store.setState(initial(), true);
