import { useEffect } from "react";

type SwipePoint = {
  x: number;
  y: number;
};

const MOBILE_BREAKPOINT = 760;
const EDGE_START_WIDTH = 36;
const MIN_BACK_DISTANCE = 72;

export function isMobileBackSwipe(start: SwipePoint, end: SwipePoint) {
  const horizontalDistance = end.x - start.x;
  const verticalDistance = Math.abs(end.y - start.y);
  return (
    start.x <= EDGE_START_WIDTH &&
    horizontalDistance >= MIN_BACK_DISTANCE &&
    verticalDistance <= Math.max(56, horizontalDistance * 0.6)
  );
}

export function useMobileBackSwipe(enabled: boolean, onBack: () => void) {
  useEffect(() => {
    if (!enabled) {
      return;
    }

    let start: SwipePoint | null = null;

    function onTouchStart(event: TouchEvent) {
      if (window.innerWidth > MOBILE_BREAKPOINT || event.touches.length !== 1) {
        start = null;
        return;
      }
      const touch = event.touches[0];
      start = { x: touch.clientX, y: touch.clientY };
    }

    function onTouchEnd(event: TouchEvent) {
      const origin = start;
      start = null;
      const touch = event.changedTouches[0];
      if (
        origin &&
        touch &&
        isMobileBackSwipe(origin, { x: touch.clientX, y: touch.clientY })
      ) {
        onBack();
      }
    }

    function onTouchCancel() {
      start = null;
    }

    document.addEventListener("touchstart", onTouchStart, { passive: true });
    document.addEventListener("touchend", onTouchEnd, { passive: true });
    document.addEventListener("touchcancel", onTouchCancel, { passive: true });
    return () => {
      document.removeEventListener("touchstart", onTouchStart);
      document.removeEventListener("touchend", onTouchEnd);
      document.removeEventListener("touchcancel", onTouchCancel);
    };
  }, [enabled, onBack]);
}
