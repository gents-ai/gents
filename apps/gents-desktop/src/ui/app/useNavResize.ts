import { useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";

import { NAV_WIDTH, preferences } from "@/preferences";
import { sidebar } from "./sidebarChange";

/** the rail's column, which a sidebar dragged narrow enough gives way to */
export const RAIL_WIDTH = 56;

/** The sidebar's width as its edge is dragged, from its minimum up to
    `most`, and the edge's handle. The width never goes below the minimum,
    so its rows never squeeze and wrap: dragged nearer the rail than that,
    the sidebar shows as icons only (`collapsing`), and dragged back out it
    expands again. The rail's own edge pulls the sidebar out the same way
    while it shows as icons (`onRail`). On release the width is kept for
    this device; let go while collapsing, an expanded sidebar stays icons
    only, and a rail stays as it was. */
export function useNavResize(width: number, most: number, onRail: boolean) {
  const [live, setLive] = useState<number | null>(null);
  const [collapsing, setCollapsing] = useState(false);
  const collapsingRef = useRef(false);
  const liveRef = useRef<number | null>(null);
  const drag = useRef<{
    x: number;
    from: number;
    fromRail: boolean;
    /* 1 left to right; -1 right to left, where the sidebar's edge widens it
       toward the window's left */
    dir: 1 | -1;
  } | null>(null);
  const max = Math.max(NAV_WIDTH.min, Math.min(NAV_WIDTH.max, most));
  const shown = Math.min(width, max);
  const clamp = (w: number) => Math.min(max, Math.max(NAV_WIDTH.min, w));
  const follow = (w: number | null, toRail = false) => {
    liveRef.current = w;
    collapsingRef.current = toRail;
    setLive(w);
    setCollapsing(toRail);
  };
  const onPointerDown = (e: PointerEvent<HTMLElement>) => {
    if (e.button !== 0 || drag.current) return;
    e.preventDefault();
    document.documentElement.dataset.dragging = "divider";
    const from = onRail ? RAIL_WIDTH : shown;
    const dir = getComputedStyle(e.currentTarget).direction === "rtl" ? -1 : 1;
    drag.current = { x: e.clientX, from, fromRail: onRail, dir };
    follow(from, onRail);
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: PointerEvent<HTMLElement>) => {
    const d = drag.current;
    if (!d) return;
    const to = d.from + (e.clientX - d.x) * d.dir;
    if (to < (NAV_WIDTH.min + RAIL_WIDTH) / 2) follow(RAIL_WIDTH, true);
    else follow(clamp(to));
  };
  const onPointerUp = () => {
    const d = drag.current;
    if (!d) return;
    drag.current = null;
    delete document.documentElement.dataset.dragging;
    /* the drag already draws the mode it lets go in, so this isn't morphed */
    if (collapsingRef.current) {
      if (!d.fromRail) preferences.setNav("collapsed");
    } else {
      preferences.setNavWidth(clamp(liveRef.current ?? shown));
      if (d.fromRail) preferences.setNav("expanded");
    }
    follow(null);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    const step = e.shiftKey ? 64 : 16;
    /* the arrows move the edge the way they point, so right to left the
       left arrow widens */
    const rtl = getComputedStyle(e.currentTarget).direction === "rtl";
    const widen = rtl ? "ArrowLeft" : "ArrowRight";
    const narrow = rtl ? "ArrowRight" : "ArrowLeft";
    if (onRail) {
      if (e.key !== widen) return;
      e.preventDefault();
      sidebar.setNav("expanded");
      return;
    }
    if (e.key === narrow && shown <= NAV_WIDTH.min) {
      e.preventDefault();
      sidebar.setNav("collapsed");
      return;
    }
    const to =
      e.key === widen
        ? shown + step
        : e.key === narrow
          ? shown - step
          : e.key === "Home"
            ? NAV_WIDTH.min
            : e.key === "End"
              ? max
              : null;
    if (to === null) return;
    e.preventDefault();
    preferences.setNavWidth(clamp(to));
  };
  return {
    width: live ?? shown,
    dragging: live !== null,
    collapsing,
    handleProps: {
      role: "separator" as const,
      "aria-orientation": "vertical" as const,
      "aria-valuemin": RAIL_WIDTH,
      "aria-valuemax": max,
      "aria-valuenow": Math.round(live ?? (onRail ? RAIL_WIDTH : shown)),
      tabIndex: 0,
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel: onPointerUp,
      onLostPointerCapture: onPointerUp,
      onKeyDown,
      onDoubleClick: () => {
        preferences.setNavWidth(NAV_WIDTH.initial);
        if (onRail) sidebar.setNav("expanded");
      },
    },
  };
}
