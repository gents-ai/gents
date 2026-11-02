import { useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";

import { NAV_WIDTH, preferences } from "@/preferences";

/** the rail's column, which a sidebar dragged narrow enough gives way to */
export const RAIL_WIDTH = 56;

/** The expanded sidebar's width as its edge is dragged, from its minimum
    up to `most`, and the edge's handle. The width never goes below the
    minimum, so its rows never squeeze and wrap: dragged nearer the rail
    than that, the sidebar shows as icons only (`collapsing`) and expands
    again when dragged back out. On release the width is kept for this
    device; let go while collapsing, the sidebar stays icons only and the
    width stays for next time. */
export function useNavResize(width: number, most: number) {
  const [live, setLive] = useState<number | null>(null);
  const [collapsing, setCollapsing] = useState(false);
  const collapsingRef = useRef(false);
  const liveRef = useRef<number | null>(null);
  const drag = useRef<{ x: number; from: number } | null>(null);
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
    drag.current = { x: e.clientX, from: shown };
    follow(shown);
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: PointerEvent<HTMLElement>) => {
    const d = drag.current;
    if (!d) return;
    const to = d.from + e.clientX - d.x;
    if (to < (NAV_WIDTH.min + RAIL_WIDTH) / 2) follow(RAIL_WIDTH, true);
    else follow(clamp(to));
  };
  const onPointerUp = () => {
    if (!drag.current) return;
    drag.current = null;
    delete document.documentElement.dataset.dragging;
    if (collapsingRef.current) preferences.setNav("collapsed");
    else preferences.setNavWidth(clamp(liveRef.current ?? shown));
    follow(null);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    const step = e.shiftKey ? 64 : 16;
    const to =
      e.key === "ArrowRight"
        ? shown + step
        : e.key === "ArrowLeft"
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
      "aria-valuemin": NAV_WIDTH.min,
      "aria-valuemax": max,
      "aria-valuenow": Math.round(clamp(live ?? shown)),
      tabIndex: 0,
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel: onPointerUp,
      onLostPointerCapture: onPointerUp,
      onKeyDown,
      onDoubleClick: () => preferences.setNavWidth(NAV_WIDTH.initial),
    },
  };
}
