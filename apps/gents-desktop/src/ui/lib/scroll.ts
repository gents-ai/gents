import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { scrollParent } from "@gents/ui/conversation";

/* Hold something still across a layout change. Folding a long block away
   moves everything below it, and a reader who collapsed something ends up
   somewhere they never chose. Measure before the change, correct after it.

   Holding the top still is only right when that top is on screen. A reader
   deep inside a folding block has no anchor at all — what they were
   reading is gone — so the block itself is brought back into view rather
   than dropping them wherever the arithmetic lands, which is always below
   where they started.

   scrollParent comes from the kit, where the step's own hold already needed
   it: two copies of the same six lines is how they drift. */
/* how far the app's own chrome floats over the top of the scroller */
const inset = (node: HTMLElement) => {
  const declared = parseFloat(
    getComputedStyle(node).getPropertyValue("--step-scroll-inset"),
  );
  return Number.isFinite(declared) ? declared : 8;
};

/* call before the state change; call the result after it */
export function anchor(node: HTMLElement | null): () => void {
  const scroller = node && scrollParent(node);
  if (!node || !scroller) return () => {};
  const before = node.getBoundingClientRect().top;
  return () =>
    requestAnimationFrame(() => {
      const top = scroller.getBoundingClientRect().top + inset(node);
      const after = node.getBoundingClientRect().top;
      /* the reader was inside it: its top was above the view, so there is
         nothing of theirs left to hold. Put the block back under them. */
      if (before < top) scroller.scrollTop += after - top;
      else if (after !== before) scroller.scrollTop += after - before;
    });
}

const FOLLOW_THRESHOLD_PX = 64;

export function scrollViewport(owner: HTMLDivElement | null) {
  return owner?.querySelector<HTMLElement>("[data-slot=scroll-area-viewport]") ?? null;
}

function isNearTip(viewport: HTMLElement) {
  return (
    viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight <
    FOLLOW_THRESHOLD_PX
  );
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

/**
 * Keep a scroller at its foot as its content grows, unless the reader has
 * scrolled away. Growth is observed on the content box, so whatever grows,
 * a new row or text still being revealed, keeps the foot in view. Intent is
 * the reader's last scroll position, recorded before growth: measured after
 * a large chunk lands, the new height alone would read as having scrolled
 * up. A new subject starts at its foot.
 */
export function useFollowTail(scroller: HTMLElement | null, subject: string | null) {
  const shouldFollow = useRef(true);
  const [atBottom, setAtBottom] = useState(true);

  useLayoutEffect(() => {
    shouldFollow.current = true;
    setAtBottom(true);
    if (!scroller || !subject) return;
    const pin = () => {
      if (!shouldFollow.current) return;
      scroller.scrollTop = scroller.scrollHeight;
      setAtBottom(true);
    };
    pin();
    const observer = new ResizeObserver(pin);
    for (const child of Array.from(scroller.children)) observer.observe(child);
    const observeIntent = () => {
      const nearTip = isNearTip(scroller);
      shouldFollow.current = nearTip;
      setAtBottom(nearTip);
    };
    scroller.addEventListener("scroll", observeIntent, { passive: true });
    return () => {
      observer.disconnect();
      scroller.removeEventListener("scroll", observeIntent);
    };
  }, [scroller, subject]);

  const toBottom = () => {
    if (!scroller) return;
    /* A smooth scroll is abandoned the moment anything else writes to the
       scroller, and a long transcript writes constantly: every scroll event
       on the way down re-renders hundreds of rows, and the animation is
       dropped halfway or never starts. The button then plays its press and
       does nothing, which is worse than arriving without ceremony.

       So a short way is animated and a long way is not, and either way the
       foot is claimed again on the next frame, after whatever render the
       click set off has landed. */
    const distance = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    shouldFollow.current = true;
    scroller.scrollTo({
      top: scroller.scrollHeight,
      behavior: distance > scroller.clientHeight * 2 ? "auto" : "smooth",
    });
    requestAnimationFrame(() => {
      if (shouldFollow.current) scroller.scrollTop = scroller.scrollHeight;
    });
  };

  return { atBottom, toBottom };
}

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

/* puts the held row back where the reader had it, allowing for their own
   scrolling since; a row that left the DOM falls back to the added height */
function restore(viewport: HTMLElement, hold: Hold) {
  if (hold.row?.isConnected && hold.top !== undefined) {
    const movement = viewport.scrollTop - hold.scrollTop;
    viewport.scrollTop += hold.row.getBoundingClientRect().top - hold.top + movement;
  } else {
    viewport.scrollTop += viewport.scrollHeight - hold.height;
  }
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

  useLayoutEffect(() => {
    const held = hold.current;
    if (!held || !scroller) return;
    if (!held.landed && oldestKey !== held.oldestKey) {
      restore(scroller, held);
      held.landed = true;
    }
    if (held.settled) hold.current = null;
  }, [oldestKey, scroller, settles]);

  useEffect(() => {
    const viewport = scroller;
    if (!viewport || !subject) return;
    let disposed = false;
    let lastTop = viewport.scrollTop;
    hold.current = null;
    setLoading(false);
    const fetchOlder = async () => {
      if (
        disposed ||
        hold.current ||
        !latest.current.hasOlder ||
        viewport.scrollTop > 160
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
    const onScroll = () => {
      const upward = viewport.scrollTop < lastTop;
      lastTop = viewport.scrollTop;
      if (upward) void fetchOlder();
    };
    const onWheel = (event: WheelEvent) => {
      if (event.deltaY < 0) void fetchOlder();
    };
    let touchY: number | undefined;
    const onTouchStart = (event: TouchEvent) => {
      touchY = event.touches[0]?.clientY;
    };
    const onTouchMove = (event: TouchEvent) => {
      const nextY = event.touches[0]?.clientY;
      if (touchY !== undefined && nextY !== undefined && nextY > touchY)
        void fetchOlder();
      touchY = nextY;
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (["ArrowUp", "PageUp", "Home"].includes(event.key)) void fetchOlder();
    };
    viewport.addEventListener("scroll", onScroll, { passive: true });
    viewport.addEventListener("wheel", onWheel, { passive: true });
    viewport.addEventListener("touchstart", onTouchStart, { passive: true });
    viewport.addEventListener("touchmove", onTouchMove, { passive: true });
    viewport.addEventListener("keydown", onKeyDown);
    return () => {
      disposed = true;
      hold.current = null;
      viewport.removeEventListener("scroll", onScroll);
      viewport.removeEventListener("wheel", onWheel);
      viewport.removeEventListener("touchstart", onTouchStart);
      viewport.removeEventListener("touchmove", onTouchMove);
      viewport.removeEventListener("keydown", onKeyDown);
    };
  }, [scroller, subject]);
  return loading;
}
