/* How the open session's column is laid out around its transcript: the
   header that condenses once scrolled out, and the room the composer takes
   at the foot. */
import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { scrollParent } from "@gents/ui/conversation";

import { distanceFromFoot } from "@/lib/scroll";

/**
 * Whether the header has scrolled out of `scroller`, so a condensed one
 * sticks to the top. Give the returned ref to an element at the header's
 * end; it mounts with the session, after the screen, and is watched from
 * then.
 */
export function useHeaderScrolledOut(scroller: HTMLElement | null) {
  const [marker, setMarker] = useState<HTMLElement | null>(null);
  const [out, setOut] = useState(false);
  useEffect(() => {
    if (!marker || !scroller) return;
    const io = new IntersectionObserver(([e]) => setOut(!e!.isIntersecting), {
      root: scroller,
    });
    io.observe(marker);
    return () => io.disconnect();
  }, [marker, scroller]);
  return [out, setMarker] as const;
}

/**
 * The room the composer takes at the foot of `column`, published as its
 * `--composer-h`: give the returned ref to the composer. The composer
 * mounts with the session, not with the screen, so it is measured from a
 * callback ref rather than an effect that would run once while it was
 * still absent. The height goes on the column, not the composer: a custom
 * property inherits down, and the blocks that need to clear it are the
 * composer's siblings.
 */
export function useComposerRoom(column: RefObject<HTMLElement | null>) {
  const cleanup = useRef<(() => void) | null>(null);
  return useCallback(
    (el: HTMLDivElement | null) => {
      cleanup.current?.();
      cleanup.current = null;
      if (!el) return;
      /* what a block sticking to the foot needs is not the composer's height
         but how far its top sits above the scrollport's bottom edge. The two
         coincide only when the scroller ends where the window does, which is
         not true once the app is drawn inside a window frame. */
      const publish = () => {
        const scroller = scrollParent(el);
        const floor = scroller
          ? scroller.getBoundingClientRect().bottom
          : window.innerHeight;
        const gap = Math.max(0, Math.round(floor - el.getBoundingClientRect().top));
        /* the transcript pins itself to the foot when a session opens, and
           this measurement arrives after that: the room it reserves appears
           underneath a view that has already stopped, leaving it exactly a
           composer short of the end. A reader at the foot stays at the foot. */
        const was = scroller && distanceFromFoot(scroller);
        column.current?.style.setProperty("--composer-h", `${gap}px`);
        if (scroller && was !== null && was < 4)
          requestAnimationFrame(() => {
            scroller.scrollTop = scroller.scrollHeight;
          });
      };
      publish();
      const size = new ResizeObserver(publish);
      size.observe(el);
      /* the frame around the app resizes without the composer changing size */
      window.addEventListener("resize", publish);
      cleanup.current = () => {
        size.disconnect();
        window.removeEventListener("resize", publish);
      };
    },
    [column],
  );
}
