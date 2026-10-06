import { useEffect, useRef, useState } from "react";

import type { Route } from "@/lib/router";
import { navPreference, saveNavPreference, type NavMode } from "@/nav";
import { useDivider } from "@/lib/divider";
import { ROOMY_WINDOW, useMediaQuery } from "@/lib/media";
import { dockView } from "./dock-scope";
import { dockScope, useDockFor } from "./workspace";

/** The shell's columns for the window it is in: the rail, the pane and the
    dock, each a length the divider drives, and the person's nav choice. */
export function useShellLayout(route: Route) {
  const [nav, setNav] = useState<NavMode>(navPreference);
  const chooseNav = (mode: NavMode) => {
    saveNavPreference(mode);
    setNav(mode);
  };
  const { dock, scope: dockOwner, closeDock } = useDockFor(dockScope(route));
  const dockOpen = dockView(dock, route.name).shown;
  const shellRef = useRef<HTMLDivElement>(null);
  /* Observed on the shell itself: the window's resize event fires before the
     frame's width variable has caught up, so it would measure the old width */
  const [shellWidth, setShellWidth] = useState(0);
  useEffect(() => {
    const el = shellRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() =>
      setShellWidth(el.getBoundingClientRect().width),
    );
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const wide = useMediaQuery("(min-width: 768px)");
  const roomy = useMediaQuery(ROOMY_WINDOW);
  /* the preference is kept; a narrow window shows the rail in its place */
  const shownNav: NavMode = nav === "expanded" && !roomy ? "hover" : nav;
  const rail = wide ? (shownNav === "expanded" ? 304 : 56) : 0;
  /* the dock is a column only where the window has the room; a half-screen
     window shows it as a sheet over the pane */
  const docked = wide && roomy;
  const divider = useDivider({
    key: "gents-prototype-trace-width",
    initial: 520,
    min: 320,
    paneMin: 360,
    gap: 8,
    container: shellWidth,
    rail,
    open: docked && dockOpen,
    scope: dockOwner,
    onClosed: closeDock,
  });
  const { jumpClosed } = divider;
  /* the dock is the pane's: leaving for a route where none of its tabs
     apply closes it, and it opens again only when asked */
  useEffect(() => {
    if (dock.open && !dockView(dock, route.name).shown) {
      closeDock();
      jumpClosed();
    }
  }, [route.name, dock, jumpClosed, closeDock]);
  /* the dock stays in the grid while it settles shut */
  const dockVisible = docked && (dockOpen || divider.pos > 0);
  const dockCol = dockVisible ? divider.pos + 8 : 0;
  const paneCol = Math.max(0, shellWidth - rail - dockCol);
  /* every column is a length so the bar above and the row below share one
     set of tracks, and the divider's motion is the spring's, frame by frame */
  const columns = !wide
    ? "minmax(0,1fr)"
    : shellWidth
      ? `${rail}px ${paneCol}px ${dockCol}px`
      : `${rail}px minmax(0,1fr) ${dockCol}px`;
  return {
    shellRef,
    nav,
    chooseNav,
    shownNav,
    wide,
    docked,
    dockOpen,
    closeDock,
    divider,
    dockVisible,
    paneCol,
    columns,
  };
}

export type ShellLayout = ReturnType<typeof useShellLayout>;
