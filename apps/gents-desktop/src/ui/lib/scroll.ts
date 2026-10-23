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
    if (stick.current)
      viewport.scrollTop = viewport.scrollHeight - viewport.clientHeight;
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

/* a row asks the transcript to keep it where it is: the reader opened or
   closed something in it */
export function holdRow(el: Element | null) {
  el?.dispatchEvent(new CustomEvent("transcript:hold", { bubbles: true }));
}
