/* How the person wants the app: its theme and how the side nav shows. One
   store, kept in this browser's storage under one versioned key, so every
   control that shows a choice shows the same one. Not the app's state:
   another viewer, or another browser, has its own. */
import { useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import { browserStorage } from "./lib/storage";
import { applyTheme, systemTheme, type ThemePreference } from "./theme";

/** How the side nav shows: the rail with a flyout on hover, always expanded
    as a column that pushes the canvas, or the rail alone. */
export type NavMode = "hover" | "expanded" | "collapsed";

export type Preferences = {
  theme: ThemePreference;
  nav: NavMode;
};

const NAV_MODES: readonly string[] = ["hover", "expanded", "collapsed"];

const initial = (): Preferences => ({
  theme: systemTheme(),
  nav: "hover",
});

/** What storage held, as preferences: storage is outside the app's control
    (another build, a hand edit), so a value that is not one is the default. */
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

/** The viewer's changes to their preferences. */
export const preferences = {
  setTheme(theme: ThemePreference) {
    store.setState({ theme });
    applyTheme(theme);
  },
  setNav(nav: NavMode) {
    store.setState({ nav });
  },
};

/** Shows the chosen theme; called before the first paint. */
export const applyChosenTheme = () => applyTheme(store.getState().theme);

/** A value from the viewer's preferences; re-renders when it changes. */
export function usePreferences<T>(select: (preferences: Preferences) => T): T {
  return useStore(store, select);
}

export const useTheme = () => usePreferences((p) => p.theme);
export const useNavMode = () => usePreferences((p) => p.nav);

/** tests only: back to a first run's preferences */
export const resetPreferences = () => store.setState(initial(), true);
