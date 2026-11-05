import { useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import { FLAGS, type FlagId, type FlagSetting } from "./lib/flags";
import { browserStorage, storedStrings } from "./lib/storage";
import { applyTheme, systemTheme, type Theme, type ThemePreference } from "./theme";
import { ZOOM_STEPS } from "./lib/zoom";
import type { NotifyKind } from "./lib/notify/events";

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
  /** sessions kept at the head of the sessions list, by session key */
  pinnedSessions: string[];
  /** experimental features switched on this device, by flag id; an absent
      flag takes its default */
  flags: Record<string, FlagSetting>;
  /** the side nav's own items as arranged on this device */
  navLayout: NavLayout;
  /** the node the app opens on, or null for the one used last */
  openOn: string | null;
  /** nodes left out of the menus that offer nodes, by node DID */
  hiddenNodes: string[];
  /** agents left out of the menus that offer agents, by `agentKey` */
  hiddenAgents: string[];
  /** which kinds of thing the app's notifications tell this device about */
  notify: Record<NotifyKind, boolean>;
  /** the plugins' notification kinds turned on on this device, each as
      `pluginNotifySwitch(key, kind)`; every other kind is off */
  pluginNotify: string[];
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

const NOTIFY_ALL: Record<NotifyKind, boolean> = {
  finished: true,
  failed: true,
  question: true,
  mailbox: true,
};

function restoreNotify(
  stored: unknown,
  defaults: Record<NotifyKind, boolean>,
): Record<NotifyKind, boolean> {
  const saved = (typeof stored === "object" && stored !== null ? stored : {}) as Record<
    string,
    unknown
  >;
  const kinds = { ...defaults };
  for (const kind of Object.keys(kinds) as NotifyKind[])
    if (typeof saved[kind] === "boolean") kinds[kind] = saved[kind] as boolean;
  return kinds;
}

const initial = (): Preferences => ({
  theme: "system",
  nav: "hover",
  zoom: 1,
  sentTurn: "anchor",
  followUp: "steer",
  pinnedSessions: [],
  flags: {},
  navLayout: DEFAULT_NAV_LAYOUT,
  openOn: null,
  hiddenNodes: [],
  hiddenAgents: [],
  notify: { ...NOTIFY_ALL },
  pluginNotify: [],
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
    openOn: typeof saved.openOn === "string" ? saved.openOn : defaults.openOn,
    hiddenNodes: Array.isArray(saved.hiddenNodes)
      ? storedStrings(saved.hiddenNodes)
      : defaults.hiddenNodes,
    hiddenAgents: Array.isArray(saved.hiddenAgents)
      ? storedStrings(saved.hiddenAgents)
      : defaults.hiddenAgents,
    notify: restoreNotify(saved.notify, defaults.notify),
    pluginNotify: Array.isArray(saved.pluginNotify)
      ? storedStrings(saved.pluginNotify)
      : defaults.pluginNotify,
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
  setOpenOn(nodeDid: string | null) {
    store.setState({ openOn: nodeDid });
  },
  setNodeHidden(nodeDid: string, hidden: boolean) {
    store.setState({
      hiddenNodes: toggled(store.getState().hiddenNodes, nodeDid, hidden),
    });
  },
  setAgentHidden(nodeDid: string, agentId: string, hidden: boolean) {
    store.setState({
      hiddenAgents: toggled(
        store.getState().hiddenAgents,
        agentKey(nodeDid, agentId),
        hidden,
      ),
    });
  },
  setNotify(kind: NotifyKind, on: boolean) {
    store.setState({ notify: { ...store.getState().notify, [kind]: on } });
  },
  setPluginNotify(key: string, kind: string, on: boolean) {
    store.setState({
      pluginNotify: toggled(
        store.getState().pluginNotify,
        pluginNotifySwitch(key, kind),
        on,
      ),
    });
  },
};

/** A plugin's notification kind among the ones turned on. */
export const pluginNotifySwitch = (key: string, kind: string) => `${key} ${kind}`;

const toggled = (list: string[], key: string, on: boolean) =>
  on ? (list.includes(key) ? list : [...list, key]) : list.filter((k) => k !== key);

/** An agent's key among the hidden ones; an agent's id is its node's own. */
export const agentKey = (nodeDid: string, agentId: string) => `${nodeDid} ${agentId}`;

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
export const zoomLevel = () => store.getState().zoom;
export const useSentTurnPlacement = () => usePreferences((p) => p.sentTurn);
export const useFollowUp = () => usePreferences((p) => p.followUp);
export const usePinnedSessions = () => usePreferences((p) => p.pinnedSessions);
export const useNavLayout = () => usePreferences((p) => p.navLayout);
export const useOpenOn = () => usePreferences((p) => p.openOn);
export const openOnNode = () => store.getState().openOn;
export const useHiddenNodes = () => usePreferences((p) => p.hiddenNodes);
export const useHiddenAgents = () => usePreferences((p) => p.hiddenAgents);
export const useNotifyKinds = () => usePreferences((p) => p.notify);
/** the kinds this device is told about, for the notification owner */
export const notifyKinds = () => store.getState().notify;
export const usePluginNotify = () => usePreferences((p) => p.pluginNotify);
/** whether the person turned this plugin's kind on, on this device */
export const pluginNotifyOn = (key: string, kind: string) =>
  store.getState().pluginNotify.includes(pluginNotifySwitch(key, kind));

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
