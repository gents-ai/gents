/* Rows far from the view drawn as boxes of their own height. A long
   transcript otherwise keeps every row laid out and styled, and WebKit
   pays for all of them on every streamed chunk and every width change,
   whether or not they can be seen. */
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

/* A row comes back while still this many views away, and goes only once
   twice as far, so a row near the edge does not flicker between the two. */
const NEAR = "200% 0px";
const FAR = "400% 0px";

/** A row that must not be swapped out marks something inside it with this
    attribute: an opened reasoning block keeps its state in the row. */
export const KEEP_DRAWN = "data-keep-drawn";

type Watch = { near(at: boolean): void; far(at: boolean): void };

type TranscriptWindow = {
  watch(el: Element, watch: Watch): () => void;
  /** the height a row last had when drawn */
  heights: Map<string, number>;
};

const TranscriptWindowContext = createContext<TranscriptWindow | null>(null);

/**
 * The window over a transcript's rows, for the scroller it lives in. With
 * no IntersectionObserver (a test environment) every row stays drawn.
 */
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
    /* The observers report after the frame that moved a row into view, so a
       jump further than a view (the scrollbar dragged or clicked, Home, End)
       would paint that frame's rows as boxes. A jump draws the rows it lands
       on before that paint; small scrolls stay with the observers, which
       draw rows two views ahead. */
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

/**
 * One transcript row, drawn while it is near the view and a box of its
 * last drawn height while it is far from it. The box keeps the row's place
 * exactly, so the page's height and the reader's position do not change
 * when a row is swapped; a row changed in width since it was drawn is
 * redrawn while still far off screen, where the transcript's hold absorbs
 * the difference. A row that is always drawn (`drawn`) is never swapped.
 */
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

  /* the row's height whenever it is drawn, for the box that stands in for it */
  useLayoutEffect(() => {
    const el = ref.current;
    if (!rows || !el || !shown) return;
    /* the border box, unrounded and untransformed: a box a fraction of a
       pixel off moves everything below it when it is swapped */
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
        /* something the reader opened inside the row lives in the row */
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
