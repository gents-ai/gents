import { useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";

import { NAV_WIDTH, preferences } from "@/preferences";

/** the rail's column, which a sidebar dragged narrow enough gives way to */
export const RAIL_WIDTH = 56;

/** The expanded sidebar's width as its edge is dragged, from the rail's
    width up to `most`, and the edge's handle. On release the width is
    kept for this device; let go nearer the rail than the minimum, the
    sidebar gives way to the rail alone, and the width stays for next time. */
export function useNavResize(width: number, most: number) {
  const [live, setLive] = useState<number | null>(null);
  const liveRef = useRef<number | null>(null);
  const drag = useRef<{ x: number; from: number } | null>(null);
  const max = Math.max(NAV_WIDTH.min, Math.min(NAV_WIDTH.max, most));
  const shown = Math.min(width, max);
  const clamp = (w: number) => Math.min(max, Math.max(NAV_WIDTH.min, w));
  const follow = (w: number | null) => {
    liveRef.current = w;
    setLive(w);
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
    follow(Math.min(max, Math.max(RAIL_WIDTH, d.from + e.clientX - d.x)));
  };
  const onPointerUp = () => {
    if (!drag.current) return;
    drag.current = null;
    delete document.documentElement.dataset.dragging;
    const to = liveRef.current ?? shown;
    if (to < (NAV_WIDTH.min + RAIL_WIDTH) / 2) preferences.setNav("hover");
    else preferences.setNavWidth(clamp(to));
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
