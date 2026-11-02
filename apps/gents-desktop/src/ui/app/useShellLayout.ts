import { useEffect, useRef, useState } from "react";

import type { Route } from "@/lib/router";
import type { NavMode } from "@/preferences";
import { useNavHidden, useNavMode, useNavWidth } from "@/preferences";
import { useDivider } from "@/lib/divider";
import { ROOMY_WINDOW, useMediaQuery } from "@/lib/media";
import { dockView } from "./dock-scope";
import { RAIL_WIDTH, useNavResize } from "./useNavResize";
import { useSurfaces } from "./surfaces";
import { dockScope, useDockFor } from "./workspace";

/* the page's own minimum, and its margin and border inside its column */
const PANE_MIN = 360;
const PANE_INSET = 10;

export function useShellLayout(route: Route) {
  const nav = useNavMode();
  const { dock, scope: dockOwner, closeDock } = useDockFor(dockScope(route));
  const surfaces = useSurfaces();
  const dockOpen = dockView(dock, route.name, surfaces).shown;
  const shellRef = useRef<HTMLDivElement>(null);
  // The window's resize event fires before the frame's width variable has
  // caught up, so it would measure the old width.
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
  const shownNav: NavMode = nav === "expanded" && !roomy ? "hover" : nav;
  const navHidden = useNavHidden() && wide;
  /* what is left beside the dock once the page has its minimum, as of the
     last layout, which a dragged sidebar may not take */
  const room = useRef(Infinity);
  const navResize = useNavResize(useNavWidth(), room.current);
  const rail =
    !wide || navHidden ? 0 : shownNav === "expanded" ? navResize.width : RAIL_WIDTH;
  /* with no column beside it, the page keeps the window's margin itself */
  const paneInset = navHidden ? PANE_INSET + 8 : PANE_INSET;
  const docked = wide && roomy;
  const divider = useDivider({
    key: "gents-prototype-trace-width",
    initial: 520,
    min: 320,
    paneMin: PANE_MIN,
    gap: 8,
    container: shellWidth,
    rail,
    open: docked && dockOpen,
    scope: dockOwner,
    onClosed: closeDock,
  });
  const { jumpClosed } = divider;
  useEffect(() => {
    if (dock.open && !dockView(dock, route.name, surfaces).shown) {
      closeDock();
      jumpClosed();
    }
  }, [route.name, dock, surfaces, jumpClosed, closeDock]);
  const dockVisible = docked && (dockOpen || divider.pos > 0);
  const dockCol = dockVisible ? divider.pos + 8 : 0;
  const paneCol = Math.max(0, shellWidth - rail - dockCol);
  room.current = shellWidth ? shellWidth - dockCol - PANE_MIN - paneInset : Infinity;
  const settlingTo = docked && !divider.dragging ? divider.target : null;
  const dragFrom = useRef<number | null>(null);
  if (!(docked && divider.dragging)) dragFrom.current = null;
  else dragFrom.current ??= paneCol;
  const paneWidth =
    dragFrom.current !== null
      ? dragFrom.current
      : settlingTo === null
        ? null
        : Math.max(0, shellWidth - rail - (settlingTo > 0 ? settlingTo + 8 : 0));
  const openWidth = useRef(divider.pos);
  if (settlingTo === null && divider.pos > 0) openWidth.current = divider.pos;
  const dockWidth =
    settlingTo === null ? divider.pos : settlingTo > 0 ? settlingTo : openWidth.current;
  const columns = !wide
    ? "minmax(0,1fr)"
    : shellWidth
      ? `${rail}px ${paneCol}px ${dockCol}px`
      : `${rail}px minmax(0,1fr) ${dockCol}px`;
  return {
    shellRef,
    shownNav,
    navHidden,
    navResize,
    paneInset,
    wide,
    docked,
    dockOpen,
    closeDock,
    divider,
    dockVisible,
    paneCol,
    paneWidth,
    dockWidth,
    columns,
  };
}

export type ShellLayout = ReturnType<typeof useShellLayout>;
