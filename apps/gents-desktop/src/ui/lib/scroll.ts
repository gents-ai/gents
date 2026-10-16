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

// The owner is mounted for the hook's life.
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

// While `paused` it does not follow; when the pause ends, where the box
// actually is decides again, and it is left there, so the row just closed
// stays in view.
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

// The reader's place, held: at the foot the view follows whatever grows;
// elsewhere it holds the row under the reading line. Enforced from a
// ResizeObserver on the content, so a correction lands in the frame of the
// change. The mode changes only on the reader's own input; a row opening or
// closing announces itself with `transcript:hold` and is held while it
// moves. Scroll anchoring is off (WebKit has none; Chromium's would correct
// some changes twice). The row is found again by its key when drawn anew.
export function useFollowTail(scroller: HTMLElement | null, subject: string | null) {
  const following = useRef(true);
  const [atBottom, setAtBottom] = useState(true);
  const settleRef = useRef<(() => void) | null>(null);
  // Near the foot, following resumes only for a reader coming down to it: a
  // scroll that leaves the foot gently is still within reach of it for its
  // first frames, and pinning it back there would cancel the scroll the
  // reader is making.
  const leaving = useRef(false);

  useLayoutEffect(() => {
    following.current = true;
    setAtBottom(true);
    if (!scroller || !subject) return;
    scroller.style.overflowAnchor = "none";
    // No bounce at the ends: WebKit drops a write made during one and stops
    // drawing the scroll area until a later write takes.
    scroller.style.overscrollBehaviorY = "none";
    let anchor: { el: Element; key: string | null; offset: number } | null = null;
    const setFollowing = (next: boolean) => {
      following.current = next;
      scroller.dataset.following = String(next);
      setAtBottom(next);
    };
    scroller.dataset.following = "true";
    const top = () => scroller.getBoundingClientRect().top;
    const pin = () => {
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
      if (Math.abs(delta) >= 0.5) setScroll(scroller, scroller.scrollTop + delta);
    };
    let intentUntil = 0;
    let tailAt = -Infinity;
    let tailWay = 0;
    // How far the view moved since this module last set it, if the reader
    // moved it, else 0; a reader's move carries their intent on. Past the
    // input's window a move carrying on the reader's last direction is
    // theirs too (animated scrolls go on moving after the window ends); one
    // going up onto the foot is the browser clamping.
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
    /* Holding the scrollbar, the reader is placing the view themselves. */
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
        setScroll(scroller, scroller.scrollTop - (inset - offset));
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

  // A layout effect: a subject's cleanup takes the content's offset off
  // before the next subject's first layout, where the view is put at its foot.
  useLayoutEffect(() => {
    const viewport = scroller;
    if (!viewport || !subject) return;
    let disposed = false;
    let lastTop = viewport.scrollTop;
    let movedUpAt = -Infinity;
    let movedAt = -Infinity;
    let touching = false;
    const resting = () => !touching && performance.now() - movedAt >= TAIL_MS;
    let hidden = 0;
    let unhideTimer = 0;
    const unhide = () => {
      window.clearTimeout(unhideTimer);
      unhideTimer = 0;
      if (!hidden || !content) return;
      if (!resting() && viewport.scrollTop > 0) {
        if (!touching)
          unhideTimer = window.setTimeout(
            unhide,
            TAIL_MS - (performance.now() - movedAt),
          );
        return;
      }
      const top = viewport.scrollTop;
      content.style.removeProperty("margin-top");
      const target = Math.min(
        top + hidden,
        viewport.scrollHeight - viewport.clientHeight,
      );
      setScroll(viewport, target);
      // A write that fell short by more than WebKit's rounding was dropped;
      // the rows go back out of sight to be retried at the next rest.
      if (target - viewport.scrollTop > 2) {
        content.style.marginTop = `${-hidden}px`;
        unhideTimer = window.setTimeout(unhide, TAIL_MS);
        return;
      }
      hidden = 0;
      again.current();
    };
    land.current = (shift) => {
      if (Math.abs(shift) < 0.5) return;
      if (!content || viewport.scrollTop <= 0 || resting()) {
        setScroll(viewport, viewport.scrollTop + shift);
        return;
      }
      hidden += shift;
      content.style.marginTop = `${-hidden}px`;
      if (!unhideTimer) unhideTimer = window.setTimeout(unhide, TAIL_MS);
    };
    hold.current = null;
    setLoading(false);
    const fetchOlder = async () => {
      if (
        disposed ||
        hold.current ||
        hidden !== 0 ||
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
      } catch {
        // The paging owner reports read errors; leave scroll and retry intent intact.
      }
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
      movedAt = performance.now();
      if (hidden && viewport.scrollTop <= 0) unhide();
      const upward = viewport.scrollTop < lastTop;
      lastTop = viewport.scrollTop;
      if (upward) up();
    };
    const onWheel = (event: WheelEvent) => {
      movedAt = performance.now();
      if (event.deltaY < 0) up();
    };
    let touchY: number | undefined;
    const onTouchStart = (event: TouchEvent) => {
      touching = true;
      movedAt = performance.now();
      touchY = event.touches[0]?.clientY;
    };
    const onTouchEnd = (event: TouchEvent) => {
      if (event.touches.length) return;
      touching = false;
      movedAt = performance.now();
      if (hidden && !unhideTimer) unhideTimer = window.setTimeout(unhide, TAIL_MS);
    };
    const onTouchMove = (event: TouchEvent) => {
      movedAt = performance.now();
      const nextY = event.touches[0]?.clientY;
      if (touchY !== undefined && nextY !== undefined && nextY > touchY) up();
      touchY = nextY;
    };
    const onKeyDown = (event: KeyboardEvent) => {
      movedAt = performance.now();
      if (["ArrowUp", "PageUp", "Home"].includes(event.key)) up();
    };
    viewport.addEventListener("scroll", onScroll, { passive: true });
    viewport.addEventListener("wheel", onWheel, { passive: true });
    viewport.addEventListener("touchstart", onTouchStart, { passive: true });
    viewport.addEventListener("touchmove", onTouchMove, { passive: true });
    viewport.addEventListener("touchend", onTouchEnd, { passive: true });
    viewport.addEventListener("touchcancel", onTouchEnd, { passive: true });
    viewport.addEventListener("keydown", onKeyDown);
    return () => {
      disposed = true;
      again.current = () => {};
      land.current = () => {};
      window.clearTimeout(unhideTimer);
      if (hidden) content?.style.removeProperty("margin-top");
      hold.current = null;
      viewport.removeEventListener("scroll", onScroll);
      viewport.removeEventListener("wheel", onWheel);
      viewport.removeEventListener("touchstart", onTouchStart);
      viewport.removeEventListener("touchmove", onTouchMove);
      viewport.removeEventListener("touchend", onTouchEnd);
      viewport.removeEventListener("touchcancel", onTouchEnd);
      viewport.removeEventListener("keydown", onKeyDown);
    };
  }, [scroller, content, subject]);
  return loading;
}
