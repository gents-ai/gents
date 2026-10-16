import {
  createContext,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { flushSync } from "react-dom";

const NEAR = "200% 0px";
const FAR = "400% 0px";

export const KEEP_DRAWN = "data-keep-drawn";

type Watch = { near(at: boolean): void; far(at: boolean): void };

type TranscriptWindow = {
  watch(el: Element, watch: Watch): () => void;
  heights: Map<string, number>;
};

const TranscriptWindowContext = createContext<TranscriptWindow | null>(null);

export function TranscriptWindowProvider({
  scroller,
  session,
  children,
}: {
  scroller: HTMLElement | null;
  session: string | null;
  children: ReactNode;
}) {
  const heights = useMemo(() => new Map<string, number>(), [session]);
  const [observers, setObservers] = useState<{
    near: IntersectionObserver;
    far: IntersectionObserver;
    watches: Map<Element, Watch>;
  } | null>(null);
  useEffect(() => {
    if (!scroller || typeof IntersectionObserver !== "function") return;
    const watches = new Map<Element, Watch>();
    const near = new IntersectionObserver(
      (entries) => {
        for (const entry of entries)
          watches.get(entry.target)?.near(entry.isIntersecting);
      },
      { root: scroller, rootMargin: NEAR },
    );
    const far = new IntersectionObserver(
      (entries) => {
        for (const entry of entries)
          watches.get(entry.target)?.far(entry.isIntersecting);
      },
      { root: scroller, rootMargin: FAR },
    );
    let lastTop = scroller.scrollTop;
    const onScroll = () => {
      const jumped = Math.abs(scroller.scrollTop - lastTop) > scroller.clientHeight;
      lastTop = scroller.scrollTop;
      if (!jumped) return;
      const view = scroller.getBoundingClientRect();
      const landed: Watch[] = [];
      for (const [el, watch] of watches) {
        if ((el as HTMLElement).dataset.windowRow !== "box") continue;
        const box = el.getBoundingClientRect();
        if (box.bottom > view.top && box.top < view.bottom) landed.push(watch);
      }
      if (landed.length) flushSync(() => landed.forEach((watch) => watch.near(true)));
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    setObservers({ near, far, watches });
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      near.disconnect();
      far.disconnect();
      setObservers(null);
    };
  }, [scroller]);
  const value = useMemo<TranscriptWindow | null>(
    () =>
      observers && {
        heights,
        watch(el, watch) {
          observers.watches.set(el, watch);
          observers.near.observe(el);
          observers.far.observe(el);
          return () => {
            observers.watches.delete(el);
            observers.near.unobserve(el);
            observers.far.unobserve(el);
          };
        },
      },
    [observers, heights],
  );
  return (
    <TranscriptWindowContext.Provider value={value}>
      {children}
    </TranscriptWindowContext.Provider>
  );
}

export function WindowedRow({
  rowKey,
  drawn: alwaysDrawn = false,
  children,
  ...attributes
}: {
  rowKey: string;
  drawn?: boolean;
  children: ReactNode;
} & Record<`data-${string}`, string | undefined> & { className?: string }) {
  const rows = useContext(TranscriptWindowContext);
  const ref = useRef<HTMLDivElement>(null);
  const [far, setFar] = useState(false);
  const shown = !rows || alwaysDrawn || !far;

  useLayoutEffect(() => {
    const el = ref.current;
    if (!rows || !el || !shown) return;
    // The border box, unrounded and untransformed: a box a fraction of a
    // pixel off moves everything below it when it is swapped.
    const sizes = new ResizeObserver(([entry]) => {
      const box = entry?.borderBoxSize?.[0];
      if (box && el.childElementCount > 0) rows.heights.set(rowKey, box.blockSize);
    });
    sizes.observe(el);
    return () => sizes.disconnect();
  }, [rows, rowKey, shown]);

  useEffect(() => {
    const el = ref.current;
    if (!rows || !el || alwaysDrawn) return;
    return rows.watch(el, {
      near: (at) => {
        if (at) setFar(false);
      },
      far: (at) => {
        if (at) return;
        if (el.querySelector(`[${KEEP_DRAWN}]`)) return;
        if (!rows.heights.has(rowKey)) return;
        setFar(true);
      },
    });
  }, [rows, rowKey, alwaysDrawn]);

  return (
    <div
      ref={ref}
      {...attributes}
      data-window-row={shown ? "drawn" : "box"}
      style={shown ? undefined : { height: rows?.heights.get(rowKey) }}
    >
      {shown ? children : null}
    </div>
  );
}
