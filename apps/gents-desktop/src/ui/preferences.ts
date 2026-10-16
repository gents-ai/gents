import { useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import { browserStorage } from "./lib/storage";
import { applyTheme, systemTheme, type ThemePreference } from "./theme";

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

export const preferences = {
  setTheme(theme: ThemePreference) {
    store.setState({ theme });
    applyTheme(theme);
  },
  setNav(nav: NavMode) {
    store.setState({ nav });
  },
};

export const applyChosenTheme = () => applyTheme(store.getState().theme);

export function usePreferences<T>(select: (preferences: Preferences) => T): T {
  return useStore(store, select);
}

export const useTheme = () => usePreferences((p) => p.theme);
export const useNavMode = () => usePreferences((p) => p.nav);

export const resetPreferences = () => store.setState(initial(), true);
