/* This viewer's choices: the theme and how the side nav shows. One store,
   so every control that shows a choice shows the same one; each change is
   kept in this browser's storage under the key it has always had, and the
   theme also reaches the page and, on macOS, the window. */
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";

import { navPreference, saveNavPreference, type NavMode } from "./nav";
import { applyTheme, themePreference, type ThemePreference } from "./theme";

type Preferences = { theme: ThemePreference; nav: NavMode };

const store = createStore<Preferences>(() => ({
  theme: themePreference(),
  nav: navPreference(),
}));

/** The viewer's changes to their preferences. */
export const preferences = {
  setTheme(theme: ThemePreference) {
    applyTheme(theme);
    store.setState({ theme });
  },
  setNav(nav: NavMode) {
    saveNavPreference(nav);
    store.setState({ nav });
  },
};

/** The theme the viewer chose. */
export const useTheme = () => useStore(store, (state) => state.theme);

/** How the viewer chose to show the side nav. */
export const useNavMode = () => useStore(store, (state) => state.nav);
