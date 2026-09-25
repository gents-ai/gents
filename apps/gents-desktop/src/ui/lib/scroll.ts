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
