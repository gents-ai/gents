import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createSpring } from "./spring";

/* The divider between the pane and the dock, as one continuous number: the
   dock's width in pixels. There are no modes. Closed, the narrowest open
   width and the full width are rest points on that axis, and a drag moves
   the value freely between and past them while the cards clip rather than
   reflow. On release the value settles to the nearest rest, with the
   gesture's velocity projected forward so a flick lands where it was going.
   What is remembered is intent: the last open width. Whether the dock is
   open belongs to the workspace store; this follows it with the same spring. */
export function useDivider({
  key,
  initial,
  min,
  paneMin,
  gap,
  container,
  rail,
  open,
  scope,
  onClosed,
}: {
  key: string;
  initial: number;
  /** the dock's narrowest open width */
  min: number;
  /** the pane's narrowest width before it gives way */
  paneMin: number;
  /** the gutter each card keeps beside the divider and the edge */
  gap: number;
  /** the shell's width, 0 until measured */
  container: number;
  /** the rail column's width */
  rail: number;
  /** the store's word on whether the dock is open */
  open: boolean;
  /** whose dock this is; a change of scope shows the new one as it stands */
  scope?: string;
  /** the divider settled shut: the store should close the dock */
  onClosed: () => void;
}) {
  const [pos, setPos] = useState(0);
  const posRef = useRef(0);
  const [dragging, setDragging] = useState(false);
  const [settling, setSettling] = useState(false);
  /* the width the dock rests at when open: remembered across visits */
  const rest = useRef(
    (() => {
      try {
        const saved = Number(localStorage.getItem(key));
        return saved >= min ? saved : initial;
      } catch {
        return initial;
      }
    })(),
  );
  /* settled at the far end, with the pane hidden: kept so a window resize
     keeps it there rather than at a stale pixel value */
  const atEnd = useRef(false);

  /* the far end: the dock's column takes everything past the rail */
  const end = Math.max(0, container - rail - gap);
  /* the widest the dock may rest while the pane keeps its minimum */
  const max = Math.max(min, end - paneMin - gap);
  const clampOpen = useCallback(
    (w: number) => Math.min(max, Math.max(min, w)),
    [min, max],
  );

  const set = useCallback((x: number) => {
    posRef.current = x;
    setPos(x);
  }, []);
  const spring = useMemo(() => createSpring({ get: () => posRef.current, set }), [set]);
  const heading = useRef<number | null>(null);
  const settle = useCallback(
    (to: number, velocity = 0, then?: () => void) => {
      setSettling(true);
      heading.current = to;
      spring.to(to, velocity, () => {
        heading.current = null;
        setSettling(false);
        then?.();
      });
    },
    [spring],
  );
  const remember = useCallback(
    (w: number) => {
      rest.current = w;
      try {
        localStorage.setItem(key, String(Math.round(w)));
      } catch {
        /* storage unavailable */
      }
    },
    [key],
  );

  /* the store opens or closes the dock: the divider follows through the
     spring. Arriving at another scope's dock is not an opening or a
     closing, so it is shown as it stands, before paint. */
  const openRef = useRef(open);
  const scopeRef = useRef(scope);
  useLayoutEffect(() => {
    const arrived = scope !== scopeRef.current;
    scopeRef.current = scope;
    if (open === openRef.current) return;
    openRef.current = open;
    atEnd.current = false;
    const to = open ? clampOpen(rest.current) : 0;
    if (arrived) {
      spring.stop();
      heading.current = null;
      setSettling(false);
      set(to);
    } else if (open) settle(to);
    else if (posRef.current > 0) settle(0);
  }, [open, scope, settle, clampOpen, spring, set]);
  /* the store closed it while it could not be seen (leaving the route): no motion */
  const jumpClosed = useCallback(() => {
    spring.stop();
    setSettling(false);
    atEnd.current = false;
    set(0);
  }, [spring, set]);

  /* the window changed size: keep the far end at the far end, and a resting
     width inside the range */
  useEffect(() => {
    if (dragging || spring.running() || posRef.current === 0) return;
    if (atEnd.current) set(end);
    else if (posRef.current > max || posRef.current < min)
      set(clampOpen(posRef.current));
  }, [end, max, min, dragging, spring, set, clampOpen]);

  const drag = useRef<{
    startX: number;
    startPos: number;
    samples: { t: number; x: number }[];
  } | null>(null);
  const onPointerDown = (e: React.PointerEvent<HTMLElement>) => {
    if (e.button !== 0 || drag.current) return;
    /* WebKit starts a text selection on any press it is not told to skip,
       and drags it across both panes with the divider; Chromium happens to
       suppress it under pointer capture */
    e.preventDefault();
    getSelection()?.removeAllRanges();
    document.documentElement.dataset.dragging = "divider";
    spring.stop();
    setSettling(false);
    drag.current = {
      startX: e.clientX,
      startPos: posRef.current,
      samples: [{ t: performance.now(), x: e.clientX }],
    };
    setDragging(true);
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: React.PointerEvent<HTMLElement>) => {
    const d = drag.current;
    if (!d) return;
    const now = performance.now();
    d.samples.push({ t: now, x: e.clientX });
    /* the last 80ms are what the release's velocity is read from */
    while (d.samples.length > 2 && now - d.samples[0]!.t > 80) d.samples.shift();
    /* the dock sits to the right of the handle: dragging left widens it */
    atEnd.current = false;
    set(Math.max(0, Math.min(end, d.startPos + (d.startX - e.clientX))));
  };
  const onPointerUp = () => {
    const d = drag.current;
    if (!d) return;
    drag.current = null;
    setDragging(false);
    delete document.documentElement.dataset.dragging;
    const first = d.samples[0]!;
    const last = d.samples[d.samples.length - 1]!;
    const dt = (last.t - first.t) / 1000;
    /* px per second, positive toward widening; a pause before letting go
       means the gesture had stopped, whatever it did before */
    const paused = performance.now() - last.t > 80;
    const velocity = !paused && dt > 0 ? (first.x - last.x) / dt : 0;
    /* where the gesture was heading, a short moment on; bounded, so an
       extreme velocity can fling past one rest point but not further */
    const projected = posRef.current + Math.max(-240, Math.min(240, velocity * 0.12));
    const x = posRef.current;
    if (x < min) {
      const target = projected < min / 2 ? 0 : min;
      settle(target, velocity, () => {
        if (target === 0) onClosed();
        else remember(target);
      });
      return;
    }
    if (x > max) {
      const toEnd = projected > (max + end) / 2;
      atEnd.current = toEnd;
      settle(toEnd ? end : max, velocity, () => {
        if (!toEnd) remember(max);
      });
      return;
    }
    remember(x);
  };
  const onKeyDown = (e: React.KeyboardEvent<HTMLElement>) => {
    const step = e.shiftKey ? 64 : 16;
    /* keys held down arrive faster than the spring moves: step from where
       it is heading, not from where it is */
    const from = atEnd.current ? max : clampOpen(heading.current ?? posRef.current);
    const target =
      e.key === "ArrowLeft"
        ? clampOpen(from + step)
        : e.key === "ArrowRight"
          ? clampOpen(from - step)
          : e.key === "Home"
            ? min
            : e.key === "End"
              ? max
              : null;
    if (target === null) return;
    e.preventDefault();
    atEnd.current = false;
    settle(target, 0, () => remember(target));
  };

  /* the pane's tab: back from the far end to the remembered width */
  const showPane = useCallback(() => {
    atEnd.current = false;
    settle(clampOpen(rest.current));
  }, [settle, clampOpen]);

  return {
    /** the dock's width now, animated */
    pos,
    min,
    max,
    end,
    dragging,
    /** a settle is under way */
    settling,
    /** resting at the far end with the pane hidden */
    paneHidden: atEnd.current && !dragging && !settling && pos >= end - 0.5,
    /** 0 inside the range, rising to 1 as the dock takes the pane's last stretch */
    toEnd: end > max ? Math.max(0, Math.min(1, (pos - max) / (end - max))) : 0,
    showPane,
    jumpClosed,
    handleProps: {
      role: "separator" as const,
      "aria-orientation": "vertical" as const,
      "aria-valuemin": min,
      "aria-valuemax": max,
      "aria-valuenow": Math.round(Math.min(max, Math.max(min, pos))),
      tabIndex: 0,
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel: onPointerUp,
      /* a release outside the window can arrive only as the capture ending */
      onLostPointerCapture: onPointerUp,
      onKeyDown,
    },
  };
}
