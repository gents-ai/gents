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

/** How far a scroller sits above its foot. */
export const distanceFromFoot = (scroller: HTMLElement) =>
  scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;

/* a box this close to an edge counts as at it */
const EDGE_PX = 4;

/**
 * Whether content is hidden past either edge of the scroll area inside
 * `owner`, for a fade that says so: measured as it scrolls, and as it or
 * its content changes size. The owner is mounted for the hook's life.
 */
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

/**
 * Keep the scroll area inside `owner` at its newest row as `rows` grows,
 * while the reader leaves it at its foot. While `paused` (a row the reader
 * opened grows the box without a scroll) it does not follow; when the pause
 * ends, where the box actually is decides again, and it is left there, so
 * the row just closed stays in view. Off while `enabled` is false.
 */
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

/**
 * The scroller inside an owner element, as state: give `owner` to the
 * element as its ref. A scroller that mounts renders after the screen does
 * (a session opens behind a loader) is found when it mounts, and everything
 * keyed on it starts then.
 */
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

/* the position this module last set or read a reader's place at, per scroller */
const settled = new WeakMap<Element, number>();
/* every write this module makes to a transcript's position */
function setScroll(el: HTMLElement, top: number) {
  el.scrollTop = top;
  settled.set(el, el.scrollTop);
}

/* a reader who brings the view this close to the foot is following again */
const REPIN_PX = 24;
/* how long after a wheel, touch or key a scroll still counts as the reader's */
const INTENT_MS = 300;
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
/* rows that can be held: a step, a group or item, a transcript row */
const HOLDABLE = "[data-step-key],[data-anchor-key],[data-timeline-key]";
const KEY_ATTRIBUTES = ["data-step-key", "data-anchor-key", "data-timeline-key"];

/* how far the app's own chrome floats over the top of the scroller, as the
   held element sees it */
const insetAt = (el: Element) => {
  const declared = parseFloat(
    getComputedStyle(el).getPropertyValue("--step-scroll-inset"),
  );
  return Number.isFinite(declared) ? declared : 8;
};

/* a spinner's frames are drawn in a box sealed off from layout
   (`contain: strict`): a new frame is not a change to the content */
const decorative = (node: Node) =>
  (node instanceof Element ? node : node.parentElement)?.closest(
    "[data-slot=ascii-loader]",
  ) != null;

const keyOf = (el: Element) =>
  KEY_ATTRIBUTES.map((name) => el.getAttribute(name)).find((key) => key != null) ??
  null;

const quoted = (value: string) => `"${value.replace(/["\\]/g, "\\$&")}"`;

/** Says `el` is about to open or close, as the kit's steps do: the scroller
    holding the reader's place keeps it still while its content moves. */
export function holdRow(el: Element | null) {
  el?.dispatchEvent(new CustomEvent("transcript:hold", { bubbles: true }));
}

/**
 * The reader's place in a scroller, held. Two modes, and the reader chooses
 * between them: at the foot, the view follows whatever grows; anywhere else
 * it holds still, keeping the row under the reading line where it is on
 * screen whether the change is above it, below it, or in it.
 *
 * Both are enforced from a ResizeObserver on the content, which runs after
 * layout and before paint, so a correction lands in the frame of the change.
 * The mode changes only on the reader's own input (a wheel, a touch, a key
 * that scrolls, a scrollbar drag): a scroll with none behind it (the
 * browser clamping as content shrinks, WebKit resetting the position while
 * a session loads) says nothing about where they want to be. A row opening
 * or closing announces itself with a `transcript:hold` event first, and is
 * held while it moves.
 *
 * The row is found again by its key when it is drawn anew. The browser's own
 * scroll anchoring is off: WebKit has none, and Chromium's corrected some of
 * the same changes a second time. A new subject starts at its foot.
 */
export function useFollowTail(scroller: HTMLElement | null, subject: string | null) {
  const following = useRef(true);
  /* shown as "at the foot" exactly when following: the way back is offered
     the moment the view stops coming to the reader */
  const [atBottom, setAtBottom] = useState(true);
  const settleRef = useRef<(() => void) | null>(null);
  /* The reader's last move was up the page. Near the foot, following
     resumes only for a reader coming down to it: a scroll that leaves the
     foot gently (WebKit eases a held arrow key in a few pixels at a time)
     is still within reach of it for its first frames, and pinning it back
     there would cancel the scroll the reader is making. */
  const leaving = useRef(false);

  useLayoutEffect(() => {
    following.current = true;
    setAtBottom(true);
    if (!scroller || !subject) return;
    scroller.style.overflowAnchor = "none";
    /* no bounce at the ends: WebKit drops a write to the position made
       during one, and stops drawing the scroll area until a later write takes */
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
    /* A row inside a nested scroller (a group's own box) moves as that box
       scrolls; holding it would move the whole transcript after it. Inside
       a box, the box's row is what is held. */
    const holdable = (el: Element | null): Element | null => {
      let at = el?.closest(HOLDABLE) ?? null;
      while (at && at.closest("[data-slot=scroll-area-viewport]") !== scroller)
        at = at.parentElement?.closest(HOLDABLE) ?? null;
      return at;
    };
    /* the row on the reading line, or the first below it when the line falls
       in a gap */
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
      /* Moved since the row was taken, by nothing this module did, while
         the reader is scrolling: their scroll is under way and its event
         has not come yet. That move is theirs, not a shift to undo (WebKit
         stops a held arrow key's scroll at any write). Without the reader's
         input, it is the browser clamping, and is undone. */
      const since = scroller.scrollTop - (settled.get(scroller) ?? scroller.scrollTop);
      if (since !== 0 && performance.now() <= intentUntil) {
        anchor.offset -= since;
        settled.set(scroller, scroller.scrollTop);
      }
      const delta = el.getBoundingClientRect().top - top() - anchor.offset;
      if (Math.abs(delta) >= 0.5) setScroll(scroller, scroller.scrollTop + delta);
    };
    let intentUntil = 0;
    leaving.current = false;
    /* holding the scrollbar, the reader is placing the view themselves */
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
    /* A change to the content is reconciled right after it is made, before
       paint, whether or not it changed the content's size: WebKit clamps the
       position partway through an update that replaces or moves rows, and
       an update that ends at the same height is never reported as a resize.
       The content itself mounts with the subject's first read. */
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
    /* Moving up the page is leaving the foot. Following stops at the input
       itself, before a content change can pin the view back down ahead of
       the scroll the input is about to make. */
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
        /* a drag has no direction to read: where it lets go decides */
        dragging = true;
        leaving.current = false;
      }
    };
    const onPointerUp = () => {
      if (dragging) intend();
      dragging = false;
    };
    const onScroll = () => {
      /* An animated scroll (WebKitGTK eases one wheel over 400-600 ms) goes
         on moving the view after its input's window ends: each move of its
         own carries the reader's intent on, so its tail is theirs too. A
         position this module wrote is not such a move. */
      if (
        performance.now() <= intentUntil &&
        scroller.scrollTop !== settled.get(scroller)
      )
        intend();
      if (!dragging && performance.now() > intentUntil) return;
      setFollowing(!leaving.current && distanceFromFoot(scroller) <= REPIN_PX);
      if (!following.current) capture();
    };
    /* A row the reader opens or closes says so first. It is held while the
       content moves under it, and following stops: opening something is
       reading. A row whose top hides under the floating header is brought
       just below it first, so its head stays on screen. */
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
    scroller.addEventListener("wheel", onWheel, { passive: true });
    scroller.addEventListener("touchstart", onTouchStart, { passive: true });
    scroller.addEventListener("touchmove", onTouchMove, { passive: true });
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

  /* straight there: a smooth scroll is abandoned by the first write the
     stream makes, and following takes over as soon as it arrives */
  const toBottom = useCallback(() => {
    if (!scroller) return;
    following.current = true;
    leaving.current = false;
    scroller.dataset.following = "true";
    setAtBottom(true);
    setScroll(scroller, scroller.scrollHeight - scroller.clientHeight);
  }, [scroller]);

  /* for a change the content box does not show (the room kept for the
     composer is padding), settled in the frame it happens */
  const settle = useCallback(() => settleRef.current?.(), []);

  return { atBottom, toBottom, settle };
}

/* An older page is asked for while the reader is this many views from the
   top, so it lands above them before they reach it; a page read takes a
   bridge round trip. */
const OLDER_AHEAD_VIEWS = 3;
/* and never less than this, for a pane only a few lines tall */
const OLDER_AHEAD_PX = 160;
/* moving up this recently, the next page follows the one that landed */
const OLDER_INTENT_MS = 1000;
/* A page that lands while the view is moving is not put back with a write
   to the position: WebKit drops a write made while its own scrolling (a
   fling's momentum, a held key's glide) is under way, and stops drawing the
   scroll area until a later write takes. Its rows are kept out of sight
   above the content's top instead, the content pulled up by their height,
   and that becomes one write once the view has rested this long, or once
   the reader reaches the top, where the view has stopped against the end
   (there is no bounce) and a write takes. Resting is no scroll, no wheel
   event and no finger on a touchscreen: a trackpad's momentum goes on
   sending wheel events after it is too slow to move the view a pixel, a
   finger held still is still scrolling, and WebKit reports `scrollend` as
   the fingers lift, before the momentum. */
const OLDER_STILL_MS = 150;

/* the row under the reader when an older page was asked for, and where it was */
type Hold = {
  row: HTMLElement | undefined;
  top: number | undefined;
  scrollTop: number;
  height: number;
  /** the first row's key when the page was asked for */
  oldestKey: string | null;
  /** the older rows have been committed and the row put back */
  landed: boolean;
  /** the load has finished */
  settled: boolean;
};

/* how far the held row has moved from where the reader had it, allowing for
   their own scrolling since; a row that left the DOM falls back to the added
   height */
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

/**
 * Load older pages when the reader moves up near the top, keeping the row
 * under them in place. The older rows arrive in a React commit whose timing
 * the loader does not control, so the row is put back in a layout effect on
 * that commit, before paint: the commit where `oldestKey`, the first row's
 * key, changes. The hold ends with the commit of the load's settling, which
 * React cannot commit ahead of the rows the load queued.
 */
export function useOlderPages(
  scroller: HTMLElement | null,
  /** the scroller's content, pulled up over rows landing mid-scroll */
  content: HTMLElement | null,
  subject: string | null,
  hasOlder: boolean,
  load: () => Promise<boolean>,
  oldestKey: string | null,
) {
  const latest = useRef({ hasOlder, load, oldestKey });
  latest.current = { hasOlder, load, oldestKey };
  const [loading, setLoading] = useState(false);
  /* advanced when a load settles, so a commit always follows it, even when
     React batched the start and end of a quick load into one */
  const [settles, setSettles] = useState(0);
  const hold = useRef<Hold | null>(null);
  /* asks for the next page if the reader is still moving up near the top */
  const again = useRef<() => void>(() => {});
  /* puts the reader's row back by this much */
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
      /* only after rows landed above the reader: a load that added nothing
         would ask again at once, for as long as their last upward move counts */
      if (held.landed && scroller.scrollHeight > held.height)
        queueMicrotask(() => again.current());
    }
  }, [oldestKey, scroller, settles]);

  /* A layout effect: a subject's cleanup takes the content's offset off
     before the next subject's first layout, where the view is put at its
     foot. */
  useLayoutEffect(() => {
    const viewport = scroller;
    if (!viewport || !subject) return;
    let disposed = false;
    let lastTop = viewport.scrollTop;
    let movedUpAt = -Infinity;
    let movedAt = -Infinity;
    let touching = false;
    const resting = () => !touching && performance.now() - movedAt >= OLDER_STILL_MS;
    /* the height of rows kept out of sight above the content's top */
    let hidden = 0;
    let unhideTimer = 0;
    const unhide = () => {
      window.clearTimeout(unhideTimer);
      unhideTimer = 0;
      if (!hidden || !content) return;
      if (!resting() && viewport.scrollTop > 0) {
        /* a finger on the screen: its lift asks again */
        if (!touching)
          unhideTimer = window.setTimeout(
            unhide,
            OLDER_STILL_MS - (performance.now() - movedAt),
          );
        return;
      }
      const top = viewport.scrollTop;
      content.style.removeProperty("margin-top");
      setScroll(viewport, top + hidden);
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
      if (!unhideTimer) unhideTimer = window.setTimeout(unhide, OLDER_STILL_MS);
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
      if (hidden && !unhideTimer)
        unhideTimer = window.setTimeout(unhide, OLDER_STILL_MS);
    };
    const onTouchMove = (event: TouchEvent) => {
      movedAt = performance.now();
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
