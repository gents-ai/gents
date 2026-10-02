import { useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
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
 * Preserve the reader's intent across growth of a scroller that follows its
 * tip. Measuring whether the viewport is near the tip only after a large chunk
 * lands loses that intent: the new height itself can make a previously pinned
 * viewport appear disengaged. The ref records intent on scroll and the layout
 * effect consumes that prior observation when content grows.
 *
 * The content signal has to enumerate what can grow, and any scroll event,
 * reader's or browser's, can change the mode. A reconcile from a
 * ResizeObserver on the content box, with intent read only from the reader,
 * removes both limits while keeping this signature; the transcript's
 * activity groups and folds hold their place through `anchor` until then.
 */
export function useFollowTail(
  ownerRef: RefObject<HTMLDivElement | null>,
  /** what the scroller shows; a new subject starts pinned to its tip */
  subject: string | null,
  contentSignal: string,
) {
  const shouldFollow = useRef(true);
  const openedSubject = useRef<string | null>(null);
  const [atBottom, setAtBottom] = useState(true);

  useLayoutEffect(() => {
    if (!subject) {
      openedSubject.current = null;
      shouldFollow.current = true;
      setAtBottom(true);
      return;
    }
    const viewport = scrollViewport(ownerRef.current);
    if (!viewport) return;

    const subjectChanged = openedSubject.current !== subject;
    if (subjectChanged) {
      openedSubject.current = subject;
      shouldFollow.current = true;
    }
    if (shouldFollow.current) {
      viewport.scrollTop = viewport.scrollHeight;
      setAtBottom(true);
    }
  }, [contentSignal, ownerRef, subject]);

  useEffect(() => {
    const viewport = scrollViewport(ownerRef.current);
    if (!viewport || !subject) return;
    const observeIntent = () => {
      const nearTip = isNearTip(viewport);
      shouldFollow.current = nearTip;
      setAtBottom(nearTip);
    };
    observeIntent();
    viewport.addEventListener("scroll", observeIntent, { passive: true });
    return () => viewport.removeEventListener("scroll", observeIntent);
  }, [ownerRef, subject]);

  const toBottom = () => {
    const viewport = scrollViewport(ownerRef.current);
    if (!viewport) return;
    /* A smooth scroll is abandoned the moment anything else writes to the
       scroller, and a long transcript writes constantly: every scroll event
       on the way down re-renders hundreds of rows, and the animation is
       dropped halfway or never starts. The button then plays its press and
       does nothing, which is worse than arriving without ceremony.

       So a short way is animated and a long way is not, and either way the
       foot is claimed again on the next frame, after whatever render the
       click set off has landed. */
    const distance = viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight;
    shouldFollow.current = true;
    viewport.scrollTo({
      top: viewport.scrollHeight,
      behavior: distance > viewport.clientHeight * 2 ? "auto" : "smooth",
    });
    requestAnimationFrame(() => {
      if (shouldFollow.current) viewport.scrollTop = viewport.scrollHeight;
    });
  };

  return { atBottom, toBottom };
}

/** Upward navigation loads one page at a time; mounting at the tip never does.
 * The visible row anchors prepends even if live output grows during the read. */
export function useOlderPages(
  ownerRef: RefObject<HTMLDivElement | null>,
  subject: string | null,
  hasOlder: boolean,
  load: () => Promise<boolean>,
) {
  const latest = useRef({ hasOlder, load });
  latest.current = { hasOlder, load };
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    const viewport = scrollViewport(ownerRef.current);
    if (!viewport || !subject) return;
    let disposed = false;
    let busy = false;
    let lastTop = viewport.scrollTop;
    let frame: number | null = null;
    setLoading(false);
    const fetchOlder = async () => {
      if (disposed || busy || !latest.current.hasOlder || viewport.scrollTop > 160)
        return;
      busy = true;
      const viewportTop = viewport.getBoundingClientRect().top;
      const row = Array.from(
        viewport.querySelectorAll<HTMLElement>("[data-timeline-key]"),
      ).find((node) => node.getBoundingClientRect().bottom > viewportTop);
      const top = row?.getBoundingClientRect().top;
      const height = viewport.scrollHeight;
      const scrollTop = viewport.scrollTop;
      let accepted = false;
      setLoading(true);
      try {
        accepted = await latest.current.load();
      } catch {
        // The paging owner reports read errors; leave scroll and retry intent intact.
      } finally {
        if (!disposed) {
          frame = requestAnimationFrame(() => {
            if (disposed) return;
            if (accepted) {
              if (row?.isConnected && top !== undefined) {
                const movement = viewport.scrollTop - scrollTop;
                viewport.scrollTop += row.getBoundingClientRect().top - top + movement;
              } else {
                viewport.scrollTop += viewport.scrollHeight - height;
              }
            }
            lastTop = viewport.scrollTop;
            busy = false;
            setLoading(false);
          });
        }
      }
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
      if (frame !== null) cancelAnimationFrame(frame);
      viewport.removeEventListener("scroll", onScroll);
      viewport.removeEventListener("wheel", onWheel);
      viewport.removeEventListener("touchstart", onTouchStart);
      viewport.removeEventListener("touchmove", onTouchMove);
      viewport.removeEventListener("keydown", onKeyDown);
    };
  }, [ownerRef, subject]);
  return loading;
}
