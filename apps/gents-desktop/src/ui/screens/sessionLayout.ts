import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { scrollParent } from "./scrollParent";

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
