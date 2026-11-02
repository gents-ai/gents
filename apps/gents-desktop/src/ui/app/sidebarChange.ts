import { useEffect } from "react";

import {
  preferences,
  sidebarMode,
  type NavMode,
  type SidebarMode,
} from "@/preferences";

/** an icon that moves on its own from the rail's layout to the expanded
    sidebar's when the sidebar's mode changes, named `icon:<id>` */
export const ICON_PART = "grid place-items-center [view-transition-class:morph-icon]";

type Morph = (update: () => void, which?: (key: string) => boolean) => void;

const applied: Morph = (update) => update();
let morph: Morph = applied;

/** The shell's morph, through which every change of how the sidebar shows
    is drawn, while the shell is mounted. */
export function useSidebarMorph(shell: Morph) {
  useEffect(() => {
    morph = shell;
    return () => {
      if (morph === shell) morph = applied;
    };
  }, [shell]);
}

/** Applies a change of how the sidebar shows, `to`, as one morph of the
    shell. Hiding or showing it moves the whole column, so its icons stay
    in the column's picture rather than morphing on their own. */
function change(to: SidebarMode, update: () => void) {
  const from = sidebarMode();
  if (from === to) return update();
  const columnOnly = (from === "hidden") !== (to === "hidden");
  morph(update, columnOnly ? (key) => !key.startsWith("icon:") : undefined);
}

/** Every way the sidebar's mode is changed outside a drag of its edge. */
export const sidebar = {
  setMode: (mode: SidebarMode) => change(mode, () => preferences.setSidebarMode(mode)),
  setNav: (nav: NavMode) =>
    change(sidebarMode() === "hidden" ? "hidden" : nav, () => preferences.setNav(nav)),
  toggleHidden: () => {
    const to = sidebarMode() === "hidden" ? "expanded" : "hidden";
    change(to, () => preferences.toggleNavHidden());
  },
};
