import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type RefObject,
} from "react";

export function scrollViewport(owner: HTMLElement | null) {
  return owner?.querySelector<HTMLElement>("[data-slot=scroll-area-viewport]") ?? null;
}

export const distanceFromFoot = (scroller: HTMLElement) =>
  scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;

const EDGE_PX = 4;

export function useScrollEdges(owner: RefObject<HTMLElement | null>) {
  const [edges, setEdges] = useState({ above: false, below: false });
  useEffect(() => {
    const viewport = scrollViewport(owner.current);
    if (!viewport) return;
    const measure = () => {
      const above = viewport.scrollTop > EDGE_PX;
      const below = distanceFromFoot(viewport) > EDGE_PX;
      setEdges((e) => (e.above === above && e.below === below ? e : { above, below }));
    };
    viewport.addEventListener("scroll", measure, { passive: true });
    const sizes = new ResizeObserver(measure);
    sizes.observe(viewport);
    if (viewport.firstElementChild) sizes.observe(viewport.firstElementChild);
    measure();
    return () => {
      viewport.removeEventListener("scroll", measure);
      sizes.disconnect();
    };
  }, [owner]);
  return edges;
}

export function useFollowNewest(
  owner: RefObject<HTMLElement | null>,
  { rows, paused, enabled }: { rows: number; paused: boolean; enabled: boolean },
) {
  const stick = useRef(true);
  useEffect(() => {
    const viewport = scrollViewport(owner.current);
    if (!viewport) return;
    const onScroll = () => {
      stick.current = distanceFromFoot(viewport) < EDGE_PX;
    };
    viewport.addEventListener("scroll", onScroll, { passive: true });
    return () => viewport.removeEventListener("scroll", onScroll);
  }, [owner]);
  const wasPaused = useRef(paused);
  useLayoutEffect(() => {
    const viewport = scrollViewport(owner.current);
    const resuming = wasPaused.current && !paused;
    wasPaused.current = paused;
    if (!viewport || !enabled || paused) return;
    if (resuming) {
      stick.current = distanceFromFoot(viewport) < EDGE_PX;
      return;
    }
    if (stick.current) viewport.scrollTop = viewport.scrollHeight;
  }, [owner, rows, paused, enabled]);
}

export function useScroller(): [
  HTMLElement | null,
  (owner: HTMLDivElement | null) => void,
] {
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  const owner = useCallback((element: HTMLDivElement | null) => {
    setScroller(scrollViewport(element));
  }, []);
  return [scroller, owner];
}

const settled = new WeakMap<Element, number>();
// Every write this module makes to a transcript's position. WebKit keeps
// the position in whole CSS pixels and drops a write's fraction, which
// leaves a held row up to a pixel below where it was; rounded, it lands
// within half a pixel.
function setScroll(el: HTMLElement, top: number) {
  el.scrollTop = Math.round(top);
  settled.set(el, el.scrollTop);
}

const REPIN_PX = 24;
const INTENT_MS = 300;
const TAIL_MS = 1000;
type PositionCorrection = ReturnType<typeof createPositionCorrection>;
const positionCorrections = new WeakMap<
  HTMLElement,
  {
    subject: string;
    correction: PositionCorrection;
    users: number;
  }
>();

function createPositionCorrection(viewport: HTMLElement, content: HTMLElement | null) {
  let movedAt = -Infinity;
  let touching = false;
  let hidden = 0;
  let timer = 0;
  let disposed = false;
  const listeners = new Set<() => void>();
  const resting = () => !touching && performance.now() - movedAt >= TAIL_MS;
  const schedule = (delay = TAIL_MS) => {
    window.clearTimeout(timer);
    timer = window.setTimeout(unhide, delay);
  };
  const unhide = () => {
    window.clearTimeout(timer);
    timer = 0;
    if (!hidden || !content) return;
    if (!resting() && viewport.scrollTop > 0) {
      if (!touching) schedule(TAIL_MS - (performance.now() - movedAt));
      return;
    }
    const before = viewport.scrollTop;
    content.style.removeProperty("margin-top");
    const target = Math.max(
      0,
      Math.min(before + hidden, viewport.scrollHeight - viewport.clientHeight),
    );
    setScroll(viewport, target);
    // WebKit can drop a position write even after its last motion event.
    if (Math.abs(target - viewport.scrollTop) > 2) {
      content.style.marginTop = `${-hidden}px`;
      schedule();
      return;
    }
    hidden = 0;
    for (const listener of listeners) listener();
  };
  const motion = () => {
    movedAt = performance.now();
  };
  const onScroll = () => {
    motion();
    if (hidden && viewport.scrollTop <= 0) unhide();
  };
  const onTouchStart = () => {
    touching = true;
    motion();
  };
  const onTouchEnd = (event: TouchEvent) => {
    if (event.touches.length) return;
    touching = false;
    motion();
    if (hidden) schedule();
  };
  const onKey = (event: KeyboardEvent) => {
    if (
      NAV_KEYS.has(event.key) &&
      !(event.target as Element | null)?.closest?.(
        'input, textarea, [contenteditable="true"]',
      )
    )
      motion();
  };
  viewport.addEventListener("scroll", onScroll, { passive: true });
  viewport.addEventListener("wheel", motion, { passive: true });
  viewport.addEventListener("touchstart", onTouchStart, { passive: true });
  viewport.addEventListener("touchmove", motion, { passive: true });
  viewport.addEventListener("touchend", onTouchEnd, { passive: true });
  viewport.addEventListener("touchcancel", onTouchEnd, { passive: true });
  viewport.addEventListener("keydown", onKey);
  window.addEventListener("keydown", onKey, true);
  const clear = () => {
    window.clearTimeout(timer);
    timer = 0;
    if (hidden) content?.style.removeProperty("margin-top");
    hidden = 0;
  };
  return {
    get pending() {
      return hidden !== 0;
    },
    listeners,
    clear,
    shift(delta: number) {
      if (disposed || Math.abs(delta) < 0.5) return;
      if (!content || viewport.scrollTop <= 0 || (resting() && !hidden)) {
        setScroll(viewport, viewport.scrollTop + delta);
        return;
      }
      hidden += delta;
      if (hidden) content.style.marginTop = `${-hidden}px`;
      else content.style.removeProperty("margin-top");
      if (!timer) schedule();
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      clear();
      viewport.removeEventListener("scroll", onScroll);
      viewport.removeEventListener("wheel", motion);
      viewport.removeEventListener("touchstart", onTouchStart);
      viewport.removeEventListener("touchmove", motion);
      viewport.removeEventListener("touchend", onTouchEnd);
      viewport.removeEventListener("touchcancel", onTouchEnd);
      viewport.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onKey, true);
    },
  };
}

function acquirePositionCorrection(
  viewport: HTMLElement,
  content: HTMLElement | null,
  subject: string,
) {
  let held = positionCorrections.get(viewport);
  if (held && held.subject !== subject) {
    held.correction.dispose();
    held = undefined;
  }
  if (!held) {
    held = {
      subject,
      correction: createPositionCorrection(viewport, content),
      users: 0,
    };
    positionCorrections.set(viewport, held);
  }
  held.users++;
  const owner = held;
  return {
    correction: owner.correction,
    release() {
      if (--owner.users === 0) {
        owner.correction.dispose();
        if (positionCorrections.get(viewport) === owner)
          positionCorrections.delete(viewport);
      }
    },
  };
}

/* the line a reader's eye is on, this far below the scroller's top */
const READING_LINE_PX = 72;
const NAV_KEYS = new Set([
  "ArrowUp",
  "ArrowDown",
  "PageUp",
  "PageDown",
  "Home",
  "End",
  " ",
]);
const UP_KEYS = new Set(["ArrowUp", "PageUp", "Home"]);
const HOLDABLE = "[data-step-key],[data-anchor-key],[data-timeline-key]";
const KEY_ATTRIBUTES = ["data-step-key", "data-anchor-key", "data-timeline-key"];

const insetAt = (el: Element) => {
  const declared = parseFloat(
    getComputedStyle(el).getPropertyValue("--step-scroll-inset"),
  );
  return Number.isFinite(declared) ? declared : 8;
};

const decorative = (node: Node) =>
  (node instanceof Element ? node : node.parentElement)?.closest(
    "[data-slot=ascii-loader]",
  ) != null;

const keyOf = (el: Element) =>
  KEY_ATTRIBUTES.map((name) => el.getAttribute(name)).find((key) => key != null) ??
  null;

const quoted = (value: string) => `"${value.replace(/["\\]/g, "\\$&")}"`;

export function holdRow(el: Element | null) {
  el?.dispatchEvent(new CustomEvent("transcript:hold", { bubbles: true }));
}

export function useFollowTail(scroller: HTMLElement | null, subject: string | null) {
  const following = useRef(true);
  const [atBottom, setAtBottom] = useState(true);
  const settleRef = useRef<(() => void) | null>(null);
  const leaving = useRef(false);

  useLayoutEffect(() => {
    following.current = true;
    setAtBottom(true);
    if (!scroller || !subject) return;
    scroller.style.overflowAnchor = "none";
    // No bounce at the ends: WebKit drops a write made during one and stops
    // drawing the scroll area until a later write takes.
    scroller.style.overscrollBehaviorY = "none";
    const position = acquirePositionCorrection(
      scroller,
      scroller.querySelector<HTMLElement>("[data-transcript-content]") ??
        (scroller.firstElementChild as HTMLElement | null),
      subject,
    );
    let anchor: { el: Element; key: string | null; offset: number } | null = null;
    const setFollowing = (next: boolean) => {
      following.current = next;
      scroller.dataset.following = String(next);
      setAtBottom(next);
    };
    scroller.dataset.following = "true";
    const top = () => scroller.getBoundingClientRect().top;
    const pin = () => {
      position.correction.clear();
      const foot = scroller.scrollHeight - scroller.clientHeight;
      if (Math.abs(scroller.scrollTop - foot) >= 0.5) setScroll(scroller, foot);
    };
    const holdable = (el: Element | null): Element | null => {
      let at = el?.closest(HOLDABLE) ?? null;
      while (at && at.closest("[data-slot=scroll-area-viewport]") !== scroller)
        at = at.parentElement?.closest(HOLDABLE) ?? null;
      return at;
    };
    const capture = () => {
      const box = scroller.getBoundingClientRect();
      const line = box.top + READING_LINE_PX;
      let el = holdable(
        document.elementFromPoint?.(box.left + box.width / 2, line) ?? null,
      );
      if (!el)
        el =
          Array.from(scroller.querySelectorAll(HOLDABLE)).find(
            (row) => holdable(row) === row && row.getBoundingClientRect().bottom > line,
          ) ?? null;
      anchor = el
        ? { el, key: keyOf(el), offset: el.getBoundingClientRect().top - box.top }
        : null;
      settled.set(scroller, scroller.scrollTop);
    };
    const hold = () => {
      if (!anchor) return capture();
      let el: Element | null = anchor.el;
      if (!el.isConnected && anchor.key) {
        const key = quoted(anchor.key);
        el = scroller.querySelector(
          KEY_ATTRIBUTES.map((name) => `[${name}=${key}]`).join(","),
        );
      }
      if (!el) return capture();
      anchor.el = el;
      const since = readerMoved();
      if (since !== 0) {
        anchor.offset -= since;
        settled.set(scroller, scroller.scrollTop);
      }
      const delta = el.getBoundingClientRect().top - top() - anchor.offset;
      position.correction.shift(delta);
    };
    let intentUntil = 0;
    let tailAt = -Infinity;
    let tailWay = 0;
    const readerMoved = () => {
      const since = scroller.scrollTop - (settled.get(scroller) ?? scroller.scrollTop);
      const way = Math.sign(since);
      const now = performance.now();
      if (
        way === 0 ||
        (now > intentUntil &&
          (now - tailAt > TAIL_MS ||
            way !== tailWay ||
            (way < 0 && distanceFromFoot(scroller) < 0.5)))
      )
        return 0;
      intend();
      tailAt = now;
      tailWay = way;
      return since;
    };
    leaving.current = false;
    let dragging = false;
    const reconcile = () => {
      if (dragging) return;
      if (following.current) pin();
      else hold();
    };
    settleRef.current = reconcile;
    pin();

    const sizes = new ResizeObserver(reconcile);
    const watch = () => {
      sizes.disconnect();
      for (const child of Array.from(scroller.children)) sizes.observe(child);
    };
    watch();
    const changes = new MutationObserver((records) => {
      const content = records.filter((record) => !decorative(record.target));
      if (content.length === 0) return;
      if (content.some((record) => record.target === scroller)) watch();
      reconcile();
    });
    changes.observe(scroller, { childList: true, subtree: true, characterData: true });

    const intend = () => {
      intentUntil = performance.now() + INTENT_MS;
    };
    const release = () => {
      leaving.current = true;
      if (following.current) {
        anchor = null;
        setFollowing(false);
      }
    };
    const onWheel = (event: WheelEvent) => {
      intend();
      if (event.deltaY < 0) release();
      else if (event.deltaY > 0) leaving.current = false;
    };
    let touchY: number | undefined;
    const onTouchStart = (event: TouchEvent) => {
      touchY = event.touches[0]?.clientY;
    };
    const onTouchMove = (event: TouchEvent) => {
      intend();
      const y = event.touches[0]?.clientY;
      if (touchY !== undefined && y !== undefined && y > touchY) release();
      else if (touchY !== undefined && y !== undefined && y < touchY)
        leaving.current = false;
      touchY = y;
    };
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as Element | null;
      if (target?.closest?.('input, textarea, [contenteditable="true"]')) return;
      if (!NAV_KEYS.has(event.key)) return;
      intend();
      if (UP_KEYS.has(event.key)) release();
      else leaving.current = false;
    };
    const area = scroller.parentElement;
    const onPointerDown = (event: PointerEvent) => {
      if (
        (event.target as Element | null)?.closest?.("[data-slot=scroll-area-scrollbar]")
      ) {
        dragging = true;
        leaving.current = false;
      }
    };
    const onPointerUp = () => {
      if (dragging) intend();
      dragging = false;
    };
    const onScroll = () => {
      readerMoved();
      if (!dragging && performance.now() > intentUntil) return;
      setFollowing(!leaving.current && distanceFromFoot(scroller) <= REPIN_PX);
      if (!following.current) capture();
    };
    const onHold = (event: Event) => {
      const target = event.target instanceof Element ? event.target : null;
      const el = holdable(target) ?? target;
      if (!el) return;
      setFollowing(false);
      const inset = insetAt(el);
      let offset = el.getBoundingClientRect().top - top();
      if (offset < inset) {
        position.correction.shift(-(inset - offset));
        offset = inset;
      }
      anchor = { el, key: keyOf(el), offset };
      settled.set(scroller, scroller.scrollTop);
    };
    // Chromium can apply a passive wheel or touch scroll on its compositor
    // before dispatching the event; non-passive forces the dispatch first.
    // Neither listener may call preventDefault: that would stop scrolling.
    scroller.addEventListener("wheel", onWheel, { passive: false });
    scroller.addEventListener("touchstart", onTouchStart, { passive: true });
    scroller.addEventListener("touchmove", onTouchMove, { passive: false });
    window.addEventListener("keydown", onKey);
    area?.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("pointerup", onPointerUp);
    scroller.addEventListener("scroll", onScroll, { passive: true });
    scroller.addEventListener("transcript:hold", onHold);
    return () => {
      settleRef.current = null;
      position.release();
      sizes.disconnect();
      changes.disconnect();
      scroller.removeEventListener("wheel", onWheel);
      scroller.removeEventListener("touchstart", onTouchStart);
      scroller.removeEventListener("touchmove", onTouchMove);
      window.removeEventListener("keydown", onKey);
      area?.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("pointerup", onPointerUp);
      scroller.removeEventListener("scroll", onScroll);
      scroller.removeEventListener("transcript:hold", onHold);
    };
  }, [scroller, subject]);

  const toBottom = useCallback(() => {
    if (!scroller) return;
    positionCorrections.get(scroller)?.correction.clear();
    following.current = true;
    leaving.current = false;
    scroller.dataset.following = "true";
    setAtBottom(true);
    setScroll(scroller, scroller.scrollHeight - scroller.clientHeight);
  }, [scroller]);

  const settle = useCallback(() => settleRef.current?.(), []);

  return { atBottom, toBottom, settle };
}

// An older page is asked for while the reader is this many views from the
// top, so it lands above them before they reach it; a page read takes a
// bridge round trip. Never less than OLDER_AHEAD_PX, for a pane only a few
// lines tall. Moving up this recently, the next page follows the one that
// landed.
// Rows that land mid-scroll are kept out of sight above the content's top
// (one write to the position once the view has rested or reached the top;
// a write while WebKit's own scrolling is under way stops it and one during
// fling momentum is dropped). Resting waits out TAIL_MS with no scroll, wheel
// or key event and no finger on a touchscreen, because WebKitGTK's momentum
// goes on sending wheel events and reports `scrollend` before it ends.
const OLDER_AHEAD_VIEWS = 3;
const OLDER_AHEAD_PX = 160;
const OLDER_INTENT_MS = 1000;

type Hold = {
  row: HTMLElement | undefined;
  top: number | undefined;
  scrollTop: number;
  height: number;
  oldestKey: string | null;
  landed: boolean;
  settled: boolean;
};

function shiftOf(viewport: HTMLElement, hold: Hold) {
  if (hold.row?.isConnected && hold.top !== undefined)
    return (
      hold.row.getBoundingClientRect().top -
      hold.top +
      viewport.scrollTop -
      hold.scrollTop
    );
  return viewport.scrollHeight - hold.height;
}

// Older pages load when the reader moves up near the top, keeping the row
// under them in place; the row is put back in a layout effect on the commit
// where `oldestKey` changes, before paint.
export function useOlderPages(
  scroller: HTMLElement | null,
  content: HTMLElement | null,
  subject: string | null,
  hasOlder: boolean,
  load: () => Promise<boolean>,
  oldestKey: string | null,
) {
  const latest = useRef({ hasOlder, load, oldestKey });
  latest.current = { hasOlder, load, oldestKey };
  const [loading, setLoading] = useState(false);
  const [settles, setSettles] = useState(0);
  const hold = useRef<Hold | null>(null);
  const again = useRef<() => void>(() => {});
  const land = useRef<(shift: number) => void>(() => {});

  useLayoutEffect(() => {
    const held = hold.current;
    if (!held || !scroller) return;
    if (!held.landed && oldestKey !== held.oldestKey) {
      land.current(shiftOf(scroller, held));
      held.landed = true;
    }
    if (held.settled) {
      hold.current = null;
      if (held.landed && scroller.scrollHeight > held.height)
        queueMicrotask(() => again.current());
    }
  }, [oldestKey, scroller, settles]);

  useLayoutEffect(() => {
    const viewport = scroller;
    if (!viewport || !subject) return;
    let disposed = false;
    let lastTop = viewport.scrollTop;
    let movedUpAt = -Infinity;
    const position = acquirePositionCorrection(viewport, content, subject);
    const correction = position.correction;
    const onSettled = () => again.current();
    correction.listeners.add(onSettled);
    land.current = (shift) => correction.shift(shift);
    hold.current = null;
    setLoading(false);
    const fetchOlder = async () => {
      if (
        disposed ||
        hold.current ||
        correction.pending ||
        !latest.current.hasOlder ||
        viewport.scrollTop >
          Math.max(OLDER_AHEAD_PX, OLDER_AHEAD_VIEWS * viewport.clientHeight)
      )
        return;
      const viewportTop = viewport.getBoundingClientRect().top;
      const row = Array.from(
        viewport.querySelectorAll<HTMLElement>("[data-timeline-key]"),
      ).find((node) => node.getBoundingClientRect().bottom > viewportTop);
      const held: Hold = {
        row,
        top: row?.getBoundingClientRect().top,
        scrollTop: viewport.scrollTop,
        height: viewport.scrollHeight,
        oldestKey: latest.current.oldestKey,
        landed: false,
        settled: false,
      };
      hold.current = held;
      setLoading(true);
      try {
        await latest.current.load();
      } catch {}
      if (disposed || hold.current !== held) return;
      held.settled = true;
      setLoading(false);
      setSettles((count) => count + 1);
    };
    const up = () => {
      movedUpAt = performance.now();
      void fetchOlder();
    };
    again.current = () => {
      if (performance.now() - movedUpAt < OLDER_INTENT_MS) void fetchOlder();
    };
    const onScroll = () => {
      const upward = viewport.scrollTop < lastTop;
      lastTop = viewport.scrollTop;
      if (upward) up();
    };
    const onWheel = (event: WheelEvent) => {
      if (event.deltaY < 0) up();
    };
    let touchY: number | undefined;
    const onTouchStart = (event: TouchEvent) => {
      touchY = event.touches[0]?.clientY;
    };
    const onTouchMove = (event: TouchEvent) => {
      const nextY = event.touches[0]?.clientY;
      if (touchY !== undefined && nextY !== undefined && nextY > touchY) up();
      touchY = nextY;
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (["ArrowUp", "PageUp", "Home"].includes(event.key)) up();
    };
    viewport.addEventListener("scroll", onScroll, { passive: true });
    viewport.addEventListener("wheel", onWheel, { passive: true });
    viewport.addEventListener("touchstart", onTouchStart, { passive: true });
    viewport.addEventListener("touchmove", onTouchMove, { passive: true });
    viewport.addEventListener("keydown", onKeyDown);
    return () => {
      disposed = true;
      again.current = () => {};
      land.current = () => {};
      correction.listeners.delete(onSettled);
      position.release();
      hold.current = null;
      viewport.removeEventListener("scroll", onScroll);
      viewport.removeEventListener("wheel", onWheel);
      viewport.removeEventListener("touchstart", onTouchStart);
      viewport.removeEventListener("touchmove", onTouchMove);
      viewport.removeEventListener("keydown", onKeyDown);
    };
  }, [scroller, content, subject]);
  return loading;
}
