import { useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import { FLAGS, type FlagId, type FlagSetting } from "./lib/flags";
import { browserStorage, storedStrings } from "./lib/storage";
import { applyTheme, systemTheme, type Theme, type ThemePreference } from "./theme";
import { ZOOM_STEPS } from "./lib/zoom";

export type NavMode = "hover" | "expanded" | "collapsed";
/** How the sidebar shows: one of the nav's modes, or hidden altogether
    (kept apart from the mode, which it returns to when shown again). */
export type SidebarMode = NavMode | "hidden";

/** The expanded sidebar's width, in pixels: dragged between these, and
    reset to `initial`. */
export const NAV_WIDTH = { min: 220, max: 420, initial: 304 } as const;

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
  /** the rail and sidebar put away entirely on a wide window, whatever
      `nav` is; the phone's menu is unaffected */
  navHidden: boolean;
  /** the expanded sidebar's width on this device */
  navWidth: number;
  zoom: number;
  sentTurn: SentTurnPlacement;
  followUp: FollowUp;
  /** sessions kept at the head of the sessions list, by session key */
  pinnedSessions: string[];
  /** experimental features switched on this device, by flag id; an absent
      flag takes its default */
  flags: Record<string, FlagSetting>;
  /** the side nav's own items as arranged on this device */
  navLayout: NavLayout;
};

/** The side nav's items and sections in the order chosen, and which are
    shown. An entry absent from `order` keeps its place after the ordered
    ones of its group; one absent from `shown` takes its default. Ids are
    the nav's item and section ids, and a plugin's row is kept under its
    contribution id. `recent` is how many recent sessions are listed, and
    `listed` how many items each mailbox section lists, by section id. */
export type NavLayout = {
  order: string[];
  shown: Record<string, boolean>;
  recent?: RecentCount;
  listed?: Record<string, ListedCount>;
};

export const RECENT_COUNTS = [5, 8, 12] as const;
export type RecentCount = (typeof RECENT_COUNTS)[number];

/** fewer than Recent's: these are things to act on, not to browse */
export const LISTED_COUNTS = [3, 5, 8] as const;
export type ListedCount = (typeof LISTED_COUNTS)[number];

export const DEFAULT_NAV_LAYOUT: NavLayout = { order: [], shown: {} };

const NAV_MODES: readonly string[] = ["hover", "expanded", "collapsed"];

const initial = (): Preferences => ({
  theme: "system",
  nav: "hover",
  navHidden: false,
  navWidth: NAV_WIDTH.initial,
  zoom: 1,
  sentTurn: "anchor",
  followUp: "steer",
  pinnedSessions: [],
  flags: {},
  navLayout: DEFAULT_NAV_LAYOUT,
});

/* only flags this build knows survive, so an unknown id is dropped on the
   next write of the preferences */
function knownFlags(stored: unknown): Record<string, FlagSetting> {
  if (typeof stored !== "object" || stored === null) return {};
  const saved = stored as Record<string, unknown>;
  const known: Record<string, FlagSetting> = {};
  for (const { id } of FLAGS) {
    const value = saved[id];
    if (value === "on" || value === "off") known[id] = value;
  }
  return known;
}

function restoreNavLayout(stored: unknown, defaults: NavLayout): NavLayout {
  if (typeof stored !== "object" || stored === null) return defaults;
  const saved = stored as Record<string, unknown>;
  const shown: Record<string, boolean> = {};
  if (typeof saved.shown === "object" && saved.shown !== null)
    for (const [id, value] of Object.entries(saved.shown))
      if (typeof value === "boolean") shown[id] = value;
  const recent = (RECENT_COUNTS as readonly unknown[]).includes(saved.recent)
    ? (saved.recent as RecentCount)
    : undefined;
  const listed: Record<string, ListedCount> = {};
  if (typeof saved.listed === "object" && saved.listed !== null)
    for (const [id, value] of Object.entries(saved.listed))
      if ((LISTED_COUNTS as readonly unknown[]).includes(value))
        listed[id] = value as ListedCount;
  return {
    order: Array.isArray(saved.order) ? storedStrings(saved.order) : defaults.order,
    shown,
    ...(recent ? { recent } : {}),
    ...(Object.keys(listed).length > 0 ? { listed } : {}),
  };
}

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
      saved.theme === "system" || saved.theme === "light" || saved.theme === "dark"
        ? saved.theme
        : defaults.theme,
    nav:
      typeof saved.nav === "string" && NAV_MODES.includes(saved.nav)
        ? (saved.nav as NavMode)
        : defaults.nav,
    navHidden:
      typeof saved.navHidden === "boolean" ? saved.navHidden : defaults.navHidden,
    navWidth:
      typeof saved.navWidth === "number" &&
      saved.navWidth >= NAV_WIDTH.min &&
      saved.navWidth <= NAV_WIDTH.max
        ? Math.round(saved.navWidth)
        : defaults.navWidth,
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
    pinnedSessions: Array.isArray(saved.pinnedSessions)
      ? storedStrings(saved.pinnedSessions)
      : defaults.pinnedSessions,
    flags: knownFlags(saved.flags ?? defaults.flags),
    navLayout: restoreNavLayout(saved.navLayout, defaults.navLayout),
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
  toggleNavHidden() {
    store.setState({ navHidden: !store.getState().navHidden });
  },
  /** one of the nav's modes, which shows the sidebar again, or hidden */
  setSidebarMode(mode: SidebarMode) {
    store.setState(
      mode === "hidden" ? { navHidden: true } : { nav: mode, navHidden: false },
    );
  },
  setNavWidth(width: number) {
    store.setState({
      navWidth: Math.round(Math.min(NAV_WIDTH.max, Math.max(NAV_WIDTH.min, width))),
    });
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
  /** pins at the end, or at `at`, where an undone unpin puts it back */
  setPinned(sessionKey: string, pinned: boolean, at?: number) {
    const rest = store.getState().pinnedSessions.filter((key) => key !== sessionKey);
    if (!pinned) return store.setState({ pinnedSessions: rest });
    const i = at === undefined || at < 0 ? rest.length : Math.min(at, rest.length);
    store.setState({
      pinnedSessions: [...rest.slice(0, i), sessionKey, ...rest.slice(i)],
    });
  },
  setFlag(id: FlagId, setting: "default" | FlagSetting) {
    const flags = { ...store.getState().flags };
    if (setting === "default") delete flags[id];
    else flags[id] = setting;
    store.setState({ flags });
  },
  resetFlags() {
    store.setState({ flags: {} });
  },
  setNavLayout(navLayout: NavLayout) {
    store.setState({ navLayout });
  },
  resetNavLayout() {
    store.setState({ navLayout: DEFAULT_NAV_LAYOUT });
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

/* the system's appearance, kept current so a page following it changes as
   the system does */
const system = createStore<{ theme: Theme }>(() => ({ theme: systemTheme() }));
if (typeof matchMedia === "function")
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    system.setState({ theme: systemTheme() });
    if (store.getState().theme === "system") applyTheme("system");
  });

/** The theme drawn: the chosen one, or the system's while following it. */
export const useTheme = (): Theme => {
  const chosen = usePreferences((p) => p.theme);
  const shown = useStore(system, (s) => s.theme);
  return chosen === "system" ? shown : chosen;
};
export const useThemePreference = () => usePreferences((p) => p.theme);
export const useNavMode = () => usePreferences((p) => p.nav);
export const useNavHidden = () => usePreferences((p) => p.navHidden);
export const useSidebarMode = (): SidebarMode =>
  usePreferences((p) => (p.navHidden ? "hidden" : p.nav));
export const useNavWidth = () => usePreferences((p) => p.navWidth);
export const zoomLevel = () => store.getState().zoom;
export const useSentTurnPlacement = () => usePreferences((p) => p.sentTurn);
export const useFollowUp = () => usePreferences((p) => p.followUp);
export const usePinnedSessions = () => usePreferences((p) => p.pinnedSessions);
export const useNavLayout = () => usePreferences((p) => p.navLayout);

const isOn = (flags: Record<string, FlagSetting>, id: FlagId) => {
  const setting = flags[id];
  return setting === undefined
    ? FLAGS.find((f) => f.id === id)!.default
    : setting === "on";
};

/** whether an experimental feature is on on this device, for code outside
    React; a component reads it with `useFlag` to follow changes */
export const flag = (id: FlagId) => isOn(store.getState().flags, id);
export const useFlag = (id: FlagId) => usePreferences((p) => isOn(p.flags, id));

/* the flags this window started with, which the ones that apply at launch
   keep until it relaunches */
let launchFlags = store.getState().flags;

/** whether a flag that applies at launch differs from the one this window
    launched with */
export const useRelaunchPending = () =>
  usePreferences((p) =>
    FLAGS.some((f) => f.restart && isOn(p.flags, f.id) !== isOn(launchFlags, f.id)),
  );

/** Starts the preferences afresh, as if the app launched with `flags`. */
export function resetPreferences(flags: Partial<Record<FlagId, FlagSetting>> = {}) {
  launchFlags = knownFlags(flags);
  store.setState({ ...initial(), flags: launchFlags }, true);
}
