import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { scrollParent } from "@gents/ui/conversation";

/* while a session opens, its transcript can pass over the header for a
   frame or two before it settles; a state that doesn't hold this long then
   isn't drawn */
const SETTLING_MS = 400;

export function useHeaderScrolledOut(scroller: HTMLElement | null) {
  const [marker, setMarker] = useState<HTMLElement | null>(null);
  const [out, setOut] = useState(false);
  useEffect(() => {
    if (!marker || !scroller) return;
    const opened = performance.now();
    let pending: ReturnType<typeof setTimeout> | null = null;
    const io = new IntersectionObserver(
      ([e]) => {
        const next = !e!.isIntersecting;
        if (pending !== null) clearTimeout(pending);
        pending = null;
        const wait = SETTLING_MS - (performance.now() - opened);
        if (wait <= 0) setOut(next);
        else pending = setTimeout(() => setOut(next), wait);
      },
      {
        root: scroller,
      },
    );
    io.observe(marker);
    return () => {
      if (pending !== null) clearTimeout(pending);
      io.disconnect();
    };
  }, [marker, scroller]);
  return [out, setMarker] as const;
}

export function useComposerRoom(
  column: RefObject<HTMLElement | null>,
  settle: () => void,
) {
  const cleanup = useRef<(() => void) | null>(null);
  return useCallback(
    (el: HTMLDivElement | null) => {
      cleanup.current?.();
      cleanup.current = null;
      if (!el) return;
      const publish = () => {
        const scroller = scrollParent(el);
        const floor = scroller
          ? scroller.getBoundingClientRect().bottom
          : window.innerHeight;
        const gap = Math.max(0, Math.round(floor - el.getBoundingClientRect().top));
        column.current?.style.setProperty("--composer-h", `${gap}px`);
        // WebKit applies observer-triggered resizes a frame late; settle with this measurement.
        settle();
      };
      publish();
      const size = new ResizeObserver(publish);
      size.observe(el);
      window.addEventListener("resize", publish);
      cleanup.current = () => {
        size.disconnect();
        window.removeEventListener("resize", publish);
      };
    },
    [column, settle],
  );
}
