/* The transcript's scroll owner: one virtual list over the drawn rows, and
   the only writer of the transcript's position.

   The reader is in one of three modes. Following stays at the foot. Reading
   keeps one row, by key, at its offset from the view's top through every
   change above, below or inside it. A sent turn is held at its rest place
   with room below it for the reply. After every layout change the owner
   works out where the view belongs from the list's measurements and writes
   it once; while the reader's own scroll or fling is still moving, the
   write waits until the view rests, since WebKit drops a write made during
   momentum and the reader would feel it. Older rows are added only at
   rest for the same reason. */
import {
  useCallback,
  useLayoutEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
} from "react";
import { flushSync } from "react-dom";
import type { ComponentProps } from "react";
import {
  defaultRangeExtractor,
  measureElement,
  observeElementOffset,
  observeElementRect,
  useVirtualizer,
} from "@tanstack/react-virtual";

/* where a row is read from: the reader's eye sits this far below the top */
const READING_LINE_PX = 72;
/* back within this distance of the foot, the reader follows again */
const REPIN_PX = 24;
/* a scroll this soon after the reader's input is the reader's */
const INTENT_MS = 300;
/* the view rests once no scroll has come for this long */
const REST_MS = 150;
/* how far ahead of the top older rows are asked for, in views */
const AHEAD_VIEWS = 3;
/* a sent turn settles this far below the inset, so a line of what came
   before stays in view; a tall message rises so its foot stays up */
const TURN_PEEK = 32;
const TURN_FOOT_SHARE = 1 / 3;
const TURN_FOOT_MIN_PX = 240;
const TURN_GLIDE_MS = 420;
/* a landing is settled once this many layouts in a row needed no write */
const LANDED_AFTER = 2;
const LANDING_MAX_FRAMES = 30;
/* a place still being read is waited for this long before the foot shows */
const AWAIT_PLACE_MS = 2000;
/* rows kept drawn from just above the reader's row down through the view,
   in case the list's own idea of the position is a frame behind a write
   the owner just made; rows further up are left to the list, so holding a
   row never measures rows above it for the first time */
const ABOVE_READER = 2;
const BELOW_READER = 12;
const UP_KEYS = new Set(["ArrowUp", "PageUp", "Home"]);
const DOWN_KEYS = new Set(["ArrowDown", "PageDown", " "]);
/* keys typed into a field edit it; they never scroll the transcript */
const editing = (target: EventTarget | null) =>
  target instanceof HTMLElement &&
  (target.isContentEditable || target.closest("input, textarea, select") !== null);
/* WebKit drops a position written while the reader's fling is moving; other
   engines take it */
const DEFERS_WRITES =
  typeof navigator !== "undefined" &&
  /AppleWebKit/.test(navigator.userAgent) &&
  !/Chrome|Chromium|Edg/.test(navigator.userAgent);

type Mode =
  | { kind: "follow" }
  | { kind: "read"; key: string; offset: number }
  | { kind: "turn"; key: string; offset: number }
  /* reading, with the place not yet drawn: the list draws the rows at a new
     position on its next frame, and the place is taken from those */
  | { kind: "awaiting" };

export type ReaderPlace = { key: string; offset: number };

export type TranscriptListOptions<E> = {
  scroller: HTMLElement | null;
  /* the open session: a new one opens at its foot, or at `opening` */
  subject: string | null;
  entries: readonly E[];
  keyOf: (entry: E) => string;
  /* a row's height before it is drawn, in a list this wide */
  estimateOf: (entry: E, width: number) => number;
  /* a row drawn for the first time, at the height it measured in a list
     this wide */
  onMeasured?: (entry: E, height: number, width: number) => void;
  /* the row of the newest message sent here, once it is drawn */
  sentTurn: string | null;
  anchorSentTurn: boolean;
  /* a turn doing work follows once its reply passes the room */
  turnWorks: (key: string) => boolean;
  /* the rows that open a turn, in order: the turn shortcuts step by them */
  turns: readonly string[];
  /* where to open a subject instead of its foot; "awaiting" when that
     place is still being read, and the subject shows once it is reached */
  opening?: ReaderPlace | "awaiting" | null;
  nearTop?: () => void;
  nearFoot?: () => void;
};

const insetOf = (el: Element) => {
  const declared = parseFloat(
    getComputedStyle(el).getPropertyValue("--step-scroll-inset"),
  );
  return Number.isFinite(declared) ? declared : 8;
};
const composerOf = (el: Element) =>
  parseFloat(getComputedStyle(el).getPropertyValue("--composer-h")) || 0;
const quoted = (value: string) => `"${value.replace(/["\\]/g, "\\$&")}"`;
const reducedMotion = () =>
  typeof matchMedia === "function" &&
  matchMedia("(prefers-reduced-motion: reduce)").matches;

export function useTranscriptList<E>(options: TranscriptListOptions<E>) {
  const { scroller, subject, entries, keyOf, estimateOf } = options;
  const listRef = useRef<HTMLDivElement | null>(null);
  const [margin, setMargin] = useState(0);
  const [room, setRoom] = useState(0);
  const [atBottom, setAtBottom] = useState(true);
  const [landing, setLanding] = useState(false);
  const [, drawNow] = useReducer((n: number) => n + 1, 0);

  const keys = useMemo(
    () => new Map(entries.map((entry, i) => [keyOf(entry), i])),
    [entries, keyOf],
  );
  /* everything the owner's event handlers read, current every render */
  const live = useRef({
    options,
    keys,
    virtual: null as unknown as ReturnType<
      typeof useVirtualizer<HTMLElement, HTMLDivElement>
    >,
    room,
  });
  live.current.options = options;
  live.current.keys = keys;
  live.current.room = room;
  const owner = useRef<ReturnType<typeof createOwner> | null>(null);
  /* the width rows not yet drawn are guessed in, taken as a subject opens:
     guesses that followed the width as it changed would move the length of
     every row not drawn on each frame of a resize */
  const guessWidth = useRef(0);
  const widthForGuess = () =>
    (guessWidth.current ||= listRef.current?.clientWidth || 0) || 720;
  /* what the list is told of the view. `by` is the difference the owner
     carries on the content while WebKit moves the view: the rows seen sit
     that much further down the list than the scroll position says.
     `sizing` is set while the list's own size observers deliver: rows
     drawn then would leave a size change undelivered */
  const view = useRef({ by: 0, sizing: false, redraw: () => {} });
  const virtual = useVirtualizer<HTMLElement, HTMLDivElement>({
    count: entries.length,
    getScrollElement: () => scroller,
    observeElementOffset: (instance, report) => {
      /* the rows newly in view are drawn before the frame paints */
      const tell = (offset: number, scrolling: boolean) => {
        const before = instance.range;
        report(offset + view.current.by, scrolling);
        const after = instance.range;
        if (
          before?.startIndex !== after?.startIndex ||
          before?.endIndex !== after?.endIndex
        )
          flushSync(drawNow);
      };
      view.current.redraw = () => {
        const s = instance.scrollElement;
        if (s) tell(s.scrollTop, instance.isScrolling);
      };
      return observeElementOffset(instance, tell);
    },
    observeElementRect: (instance, report) =>
      observeElementRect(instance, (rect) => {
        view.current.sizing = true;
        try {
          report(rect);
        } finally {
          view.current.sizing = false;
        }
      }),
    measureElement: (element, entry, instance) => {
      if (entry && !view.current.sizing) {
        view.current.sizing = true;
        queueMicrotask(() => {
          view.current.sizing = false;
        });
      }
      return measureElement(element, entry, instance);
    },
    estimateSize: (i) => estimateOf(entries[i]!, widthForGuess()),
    getItemKey: (i) => keyOf(entries[i]!),
    enabled: Boolean(scroller),
    initialRect: { width: 0, height: 800 },
    overscan: 6,
    rangeExtractor: (range) => {
      const drawn = defaultRangeExtractor(range);
      const key = owner.current?.heldKey();
      const at =
        key === null || key === undefined ? undefined : live.current.keys.get(key);
      if (at === undefined) return drawn;
      const around = new Set(drawn);
      const last = Math.min(range.count - 1, at + BELOW_READER);
      for (let i = Math.max(0, at - ABOVE_READER); i <= last; i += 1) around.add(i);
      return [...around].sort((a, b) => a - b);
    },
    scrollMargin: margin,
    useFlushSync: false,
    /* rows measured together are reconciled once, after the last */
    onChange: () => owner.current?.reconcileSoon(),
  });
  /* the owner keeps the reader's row itself; the list never moves the view */
  virtual.shouldAdjustScrollPositionOnItemSizeChange = () => false;
  live.current.virtual = virtual;

  owner.current ??= createOwner();
  function createOwner() {
    let mode: Mode = { kind: "follow" };
    /* the mode is published on the scroller as it changes: shortcuts and
       tests find the transcript by it */
    const setMode = (next: Mode) => {
      mode = next;
      const s = live.current.options.scroller;
      if (s) s.dataset.following = String(next.kind === "follow");
    };
    let lastWrite = -1;
    /* the last position accounted for, the owner's own or the reader's */
    let seenTop = -1;
    /* a correction the reader's fling would drop, carried as an offset of
       the list until the view rests and it can be written */
    let hidden = 0;
    /* carried on the scroller's content as a whole, so nothing inside it
       moves relative to anything else and the list measures nothing new */
    const setHidden = (next: number) => {
      hidden = next;
      const list = listRef.current;
      const content = list?.closest<HTMLElement>("[data-transcript-content]") ?? list;
      if (content) content.style.marginTop = hidden ? `${-hidden}px` : "";
      view.current.by = hidden;
      /* the content moved now and no scroll will report it: its rows are
         drawn once the caller's pass is done, which may be inside an
         animation frame, unless the list's size observers are delivering */
      if (!view.current.sizing) queueMicrotask(() => view.current.redraw());
    };
    /* a written position draws its rows before the frame paints: the
       browser reports the scroll only in the frame after. A write can come
       from any size observer on the page, so the rows wait for the next
       animation frame, which runs before the observers deliver */
    let drawPending = false;
    const drawWritten = () => {
      if (drawPending) return;
      drawPending = true;
      requestAnimationFrame(() => {
        drawPending = false;
        view.current.redraw();
      });
    };
    let lastInputAt = -Infinity;
    let lastScrollAt = -Infinity;
    /* a wheel or a finger moving the view, which WebKit counts as motion
       before any scroll event says so */
    let lastMotionAt = -Infinity;
    let touching = false;
    let dragging = false;
    let readerScrolling = false;
    let restTimer = 0;
    let quiet = 0;
    let landingFrames = -1;
    let awaitingPlace = 0;
    let glide: { from: number; start: number } | null = null;
    let roomKey: string | null = null;
    const taken = new Set<string>();

    const el = () => live.current.options.scroller;
    const resting = () =>
      !touching &&
      !dragging &&
      performance.now() - Math.max(lastScrollAt, lastMotionAt) >= REST_MS;
    const write = (top: number) => {
      const s = el();
      if (!s) return;
      lastWrite = Math.round(top);
      s.scrollTop = lastWrite;
      seenTop = s.scrollTop;
      drawWritten();
    };
    const listTop = () => {
      const s = el();
      const list = listRef.current;
      if (!s || !list) return 0;
      return (
        list.getBoundingClientRect().top - s.getBoundingClientRect().top + s.scrollTop
      );
    };
    const rowEl = (key: string) =>
      listRef.current?.querySelector<HTMLElement>(`[data-row-key=${quoted(key)}]`) ??
      null;
    /* a row's top in the scroller's content: from the page when it is
       drawn, else from the list's measurements */
    const topOf = (key: string): number | null => {
      const s = el();
      if (!s) return null;
      const drawn = rowEl(key);
      if (drawn)
        return (
          drawn.getBoundingClientRect().top -
          s.getBoundingClientRect().top +
          s.scrollTop
        );
      const { keys, virtual } = live.current;
      const index = keys.get(key);
      const measured =
        index === undefined ? undefined : virtual.measurementsCache[index];
      return measured
        ? listTop() + measured.start - virtual.options.scrollMargin
        : null;
    };
    const restOf = (key: string) => {
      const s = el();
      const drawn = rowEl(key);
      if (!s || !drawn) return 88;
      const height = drawn.getBoundingClientRect().height;
      const footLimit = Math.min(
        s.clientHeight,
        Math.max(s.clientHeight * TURN_FOOT_SHARE, TURN_FOOT_MIN_PX),
      );
      return Math.min(insetOf(drawn) + TURN_PEEK, footLimit - height);
    };
    /* the row under the reading line, held from here on */
    const capture = () => {
      const s = el();
      const list = listRef.current;
      if (!s || !list) return;
      const view = s.getBoundingClientRect();
      const line = view.top + READING_LINE_PX;
      /* the drawn rows are in order down the list: the first whose foot
         is below the line is found by halving */
      const rows = list.querySelectorAll<HTMLElement>("[data-row-key]");
      let low = 0;
      let high = rows.length;
      while (low < high) {
        const mid = (low + high) >> 1;
        if (rows[mid]!.getBoundingClientRect().bottom > line) high = mid;
        else low = mid + 1;
      }
      const row = rows[low];
      if (row) {
        const box = row.getBoundingClientRect();
        if (box.top < view.bottom) {
          setMode({
            kind: "read",
            key: row.dataset.rowKey!,
            offset: box.top - view.top,
          });
          return;
        }
      }
      setMode({ kind: "awaiting" });
      requestAnimationFrame(reconcile);
    };
    /* neighbouring drawn rows sit edge to edge: each one placed by its
       measured size, none still by its guess. A row measures before the
       list places the rows after it, so a landing waits for that too */
    const rowsAtTheirSizes = () => {
      const rows = listRef.current?.querySelectorAll<HTMLElement>("[data-row-key]");
      if (!rows) return true;
      let foot: number | null = null;
      let last = -2;
      for (const row of rows) {
        const index = Number(row.dataset.index);
        const box = row.getBoundingClientRect();
        if (foot !== null && index === last + 1 && Math.abs(box.top - foot) > 1)
          return false;
        foot = box.bottom;
        last = index;
      }
      return true;
    };
    const publishBottom = () => {
      const s = el();
      if (!s) return;
      const foot = s.scrollHeight - live.current.room - s.clientHeight;
      setAtBottom(mode.kind === "follow" || s.scrollTop >= foot - REPIN_PX);
    };
    /* the room a held turn's reply fills: the view below its rest place,
       less the composer's share, less what is already drawn below it */
    const roomFor = (key: string) => {
      const s = el();
      const list = listRef.current;
      const top = topOf(key);
      if (!s || !list || top === null) return 0;
      const below = listTop() + list.getBoundingClientRect().height - top;
      return Math.max(0, s.clientHeight - restOf(key) - composerOf(s) - below);
    };

    function reconcile() {
      const s = el();
      if (!s) return;
      /* a position not yet accounted for is the reader's: take it first,
         so a change the list reports for their scroll never undoes it */
      if (Math.abs(s.scrollTop - seenTop) >= 1) onScroll();
      if (roomKey !== null) {
        const next = Math.round(roomFor(roomKey));
        if (next !== live.current.room) setRoom(next);
        if (
          mode.kind === "turn" &&
          next === 0 &&
          live.current.options.turnWorks(mode.key)
        ) {
          setMode({ kind: "follow" });
        }
      }
      let target: number | null = null;
      if (mode.kind === "awaiting") capture();
      if (mode.kind === "awaiting") {
        /* nothing to hold until the rows at this place are drawn */
      } else if (mode.kind === "follow") {
        target = s.scrollHeight - s.clientHeight;
      } else {
        const top = topOf(mode.key);
        if (top === null) {
          /* the held row left the drawn rows: hold what is under the line */
          capture();
        } else {
          let offset = mode.offset;
          if (mode.kind === "turn" && glide) {
            const t = Math.min(1, (performance.now() - glide.start) / TURN_GLIDE_MS);
            offset = glide.from + (mode.offset - glide.from) * (1 - (1 - t) ** 3);
            if (t >= 1) glide = null;
            else requestAnimationFrame(reconcile);
          }
          target = top - offset;
        }
      }
      if (target !== null) {
        const max = s.scrollHeight - s.clientHeight;
        const next = Math.round(Math.max(0, Math.min(max, target)));
        if (Math.abs(next - s.scrollTop) >= 1) {
          if (!DEFERS_WRITES || resting() || landingFrames >= 0 || glide) {
            write(next);
            quiet = 0;
          } else {
            /* the reader is still moving: the list carries the difference
               until the view rests */
            setHidden(hidden + (next - s.scrollTop));
            scheduleRest();
          }
        } else {
          quiet += 1;
        }
      }
      if (landingFrames >= 0 && !awaitingPlace) {
        landingFrames += 1;
        if (
          (quiet >= LANDED_AFTER && rowsAtTheirSizes()) ||
          landingFrames > LANDING_MAX_FRAMES
        ) {
          landingFrames = -1;
          setLanding(false);
        } else {
          requestAnimationFrame(reconcile);
        }
      }
      publishBottom();
    }

    /* a command takes the view from wherever it is: the position as it
       stands is accounted for, and the reader's last move is no longer
       read as theirs. Otherwise a scroll the browser has yet to report,
       taken as the reader's within moments of their wheel, would undo the
       command */
    function takeOver() {
      const s = el();
      if (s) seenTop = s.scrollTop;
      lastInputAt = -Infinity;
      readerScrolling = false;
    }

    /* the carried difference written now: the list sits where it belongs
       and the reader's row is written back to its place in one step */
    function landCarried() {
      /* the position as it stands is accounted for first: taken as a scroll
         after the layout shifts, it would read the reader's place wrongly */
      const s = el();
      if (s) seenTop = s.scrollTop;
      setHidden(0);
      lastMotionAt = -Infinity;
      lastScrollAt = -Infinity;
      reconcile();
    }

    function scheduleRest() {
      window.clearTimeout(restTimer);
      restTimer = window.setTimeout(() => {
        if (!resting()) return scheduleRest();
        readerScrolling = false;
        if (hidden) setHidden(0);
        reconcile();
      }, REST_MS);
    }

    function ahead() {
      const s = el();
      if (!s) return;
      const { options } = live.current;
      const fromTop = s.scrollTop - listTop();
      if (fromTop < s.clientHeight * AHEAD_VIEWS) options.nearTop?.();
      const toFoot = s.scrollHeight - live.current.room - s.clientHeight - s.scrollTop;
      if (toFoot < s.clientHeight * AHEAD_VIEWS) options.nearFoot?.();
    }

    /* any scroll the owner did not write is motion, however small: the tail
       of a fling moves the view by less than a pixel a frame */
    function onScrollEvent() {
      const s = el();
      if (s && Math.abs(s.scrollTop - lastWrite) >= 1) lastScrollAt = performance.now();
      onScroll();
    }

    function onScroll() {
      const s = el();
      if (!s) return;
      if (Math.abs(s.scrollTop - seenTop) < 1) return;
      const moved = s.scrollTop - seenTop;
      const down = moved > 0;
      seenTop = s.scrollTop;
      if (Math.abs(s.scrollTop - lastWrite) < 1) return;
      lastScrollAt = performance.now();
      glide = null;
      /* at the top nothing more can scroll: the carried difference lands
         with the reader's row kept where it is, never taken anew from the
         layout it shifts */
      if (hidden && s.scrollTop <= 0) {
        landCarried();
        ahead();
        return;
      }
      if (readerScrolling || dragging || performance.now() - lastInputAt < INTENT_MS) {
        const foot = s.scrollHeight - live.current.room - s.clientHeight;
        const atFoot = s.scrollTop >= foot - REPIN_PX;
        /* coming down to the foot follows again; a scroll up that starts
           near the foot is the reader leaving it. A view still at the foot
           keeps following: the browser moves it there when what is below
           shrinks, and the reader leaving says so by wheel, key or touch */
        if (!(atFoot && mode.kind === "follow")) {
          if (down && atFoot && mode.kind !== "turn") setMode({ kind: "follow" });
          else if (
            mode.kind === "read" &&
            Math.abs(mode.offset - moved) < s.clientHeight
          )
            /* the reader's row moves by exactly what they scrolled: taken
               anew from the layout, a row that changed size above it in the
               same frame, as during WebKit's animated wheel, would pass as
               their move. A row that left the view gives way to the one
               under the line */
            setMode({ kind: "read", key: mode.key, offset: mode.offset - moved });
          else capture();
        }
      }
      ahead();
      publishBottom();
      scheduleRest();
    }
    const release = () => {
      lastInputAt = performance.now();
      readerScrolling = true;
      if (mode.kind !== "read") capture();
    };
    const onWheel = (event: WheelEvent) => {
      lastInputAt = performance.now();
      readerScrolling = true;
      lastMotionAt = lastInputAt;
      if (event.deltaY < 0) release();
      const s = el();
      /* wheeling at the very top moves nothing and sends no scroll: a
         carried difference lands now, and the next rows are asked for */
      if (s && s.scrollTop <= 0 && event.deltaY < 0) {
        if (hidden) landCarried();
        ahead();
      } else {
        scheduleRest();
      }
    };
    let touchY = 0;
    const onTouchStart = (event: TouchEvent) => {
      touching = true;
      readerScrolling = true;
      touchY = event.touches[0]?.clientY ?? 0;
      lastInputAt = performance.now();
    };
    const onTouchMove = (event: TouchEvent) => {
      lastInputAt = performance.now();
      lastMotionAt = lastInputAt;
      if ((event.touches[0]?.clientY ?? 0) > touchY) release();
    };
    const onTouchEnd = () => {
      touching = false;
      scheduleRest();
    };
    const onKey = (event: KeyboardEvent) => {
      if (
        event.defaultPrevented ||
        event.metaKey ||
        event.ctrlKey ||
        event.altKey ||
        editing(event.target)
      )
        return;
      if (UP_KEYS.has(event.key)) release();
      else if (event.key === "End") {
        setMode({ kind: "follow" });
        reconcile();
      } else if (DOWN_KEYS.has(event.key)) {
        lastInputAt = performance.now();
        readerScrolling = true;
      }
    };
    const onPointerDown = (event: PointerEvent) => {
      if (
        (event.target as Element | null)?.closest("[data-slot=scroll-area-scrollbar]")
      ) {
        dragging = true;
        readerScrolling = true;
        lastInputAt = performance.now();
      }
    };
    const onPointerUp = () => {
      if (!dragging) return;
      dragging = false;
      scheduleRest();
    };
    const keyOfEvent = (event: Event) =>
      (event.target as Element | null)?.closest<HTMLElement>("[data-row-key]")?.dataset
        .rowKey ?? null;
    /* a row asks to stay where it is: the reader opened or closed it */
    const onHold = (event: Event) => {
      const key = keyOfEvent(event);
      const s = el();
      const top = key === null ? null : topOf(key);
      if (!s || key === null || top === null) return;
      setMode({ kind: "read", key, offset: top - s.scrollTop });
      reconcile();
    };
    /* the turn shortcuts: from the last turn at or above the reading line
       to the one before or after it, held at the inset, drawn or not */
    const onStep = (event: Event) => {
      const s = el();
      const list = listRef.current;
      const way = (event as CustomEvent<-1 | 1>).detail;
      if (!s || !list) return;
      const { turns } = live.current.options;
      const line = s.scrollTop + READING_LINE_PX;
      let at = -1;
      for (let i = 0; i < turns.length; i += 1) {
        const top = topOf(turns[i]!);
        if (top !== null && top <= line + 0.5) at = i;
      }
      const key = turns[at + way];
      if (key === undefined) return;
      takeOver();
      setMode({ kind: "read", key, offset: insetOf(list) });
      reconcile();
      requestAnimationFrame(reconcile);
    };

    /* a change asked for while the size observers deliver is reconciled
       as one of theirs */
    let reconcileQueued = false;
    let queuedSizing = false;
    const reconcileSoon = () => {
      queuedSizing ||= view.current.sizing;
      if (reconcileQueued) return;
      reconcileQueued = true;
      queueMicrotask(() => {
        const was = view.current.sizing;
        view.current.sizing = was || queuedSizing;
        reconcileQueued = false;
        queuedSizing = false;
        try {
          reconcile();
        } finally {
          view.current.sizing = was;
        }
      });
    };

    return {
      reconcile,
      reconcileSoon,
      ahead,
      attach(s: HTMLElement) {
        s.style.overflowAnchor = "none";
        /* the view stops at either end instead of bouncing past it: a bounce
           moves the view outside the range the owner places it in, and at
           the top of history still being read it looks like the start of
           the session */
        s.style.overscrollBehaviorY = "none";
        s.dataset.following = String(mode.kind === "follow");
        s.addEventListener("scroll", onScrollEvent, { passive: true });
        s.addEventListener("wheel", onWheel, { passive: true });
        s.addEventListener("touchstart", onTouchStart, { passive: true });
        s.addEventListener("touchmove", onTouchMove, { passive: true });
        s.addEventListener("touchend", onTouchEnd, { passive: true });
        s.addEventListener("touchcancel", onTouchEnd, { passive: true });
        s.addEventListener("transcript:hold", onHold);
        s.addEventListener("transcript:step", onStep);
        window.addEventListener("keydown", onKey);
        window.addEventListener("pointerdown", onPointerDown, true);
        window.addEventListener("pointerup", onPointerUp, true);
        return () => {
          s.removeEventListener("scroll", onScrollEvent);
          s.removeEventListener("wheel", onWheel);
          s.removeEventListener("touchstart", onTouchStart);
          s.removeEventListener("touchmove", onTouchMove);
          s.removeEventListener("touchend", onTouchEnd);
          s.removeEventListener("touchcancel", onTouchEnd);
          s.removeEventListener("transcript:hold", onHold);
          s.removeEventListener("transcript:step", onStep);
          window.removeEventListener("keydown", onKey);
          window.removeEventListener("pointerdown", onPointerDown, true);
          window.removeEventListener("pointerup", onPointerUp, true);
          window.clearTimeout(restTimer);
          window.clearTimeout(awaitingPlace);
        };
      },
      /* a new subject: its foot, or the place it was left at, which may
         still be on its way: the subject stays hidden until it arrives */
      open(place: ReaderPlace | "awaiting" | null) {
        takeOver();
        glide = null;
        setHidden(0);
        roomKey = null;
        setRoom(0);
        window.clearTimeout(awaitingPlace);
        awaitingPlace = 0;
        if (place === "awaiting") {
          awaitingPlace = window.setTimeout(() => {
            awaitingPlace = 0;
            reconcile();
          }, AWAIT_PLACE_MS);
        }
        setMode(
          place && place !== "awaiting"
            ? { kind: "read", key: place.key, offset: place.offset }
            : { kind: "follow" },
        );
        /* drawn once the rows around the place are measured, so the
           guesses they start from never show */
        landingFrames = 0;
        quiet = 0;
        setLanding(true);
        reconcile();
      },
      toBottom() {
        takeOver();
        setMode({ kind: "follow" });
        reconcile();
      },
      goTo(place: ReaderPlace) {
        takeOver();
        window.clearTimeout(awaitingPlace);
        awaitingPlace = 0;
        setMode({ kind: "read", key: place.key, offset: place.offset });
        landingFrames = 0;
        quiet = 0;
        setLanding(true);
        reconcile();
      },
      /* the newest send reached its row: held at its place, or followed */
      takeSentTurn(key: string, anchor: boolean) {
        if (taken.has(key)) return;
        taken.add(key);
        takeOver();
        if (!anchor) {
          roomKey = null;
          setMode({ kind: "follow" });
          reconcile();
          return;
        }
        const s = el();
        const top = topOf(key);
        roomKey = key;
        const rest = restOf(key);
        let from = s && top !== null ? top - s.scrollTop : rest;
        /* a turn sent from far up glides in from a view away: the rows
           between are passed in one step, never drawn on the way */
        if (s)
          from = Math.max(rest - s.clientHeight, Math.min(rest + s.clientHeight, from));
        setMode({ kind: "turn", key, offset: rest });
        glide =
          reducedMotion() || Math.abs(from - rest) < 24
            ? null
            : { from, start: performance.now() };
        reconcile();
      },
      heldKey: () =>
        mode.kind === "follow" || mode.kind === "awaiting" ? null : mode.key,
      place(): ReaderPlace | null {
        if (mode.kind === "follow" || mode.kind === "awaiting") return null;
        const s = el();
        const top = topOf(mode.key);
        return s && top !== null ? { key: mode.key, offset: top - s.scrollTop } : null;
      },
    };
  }

  useLayoutEffect(() => {
    if (!scroller) return;
    return owner.current!.attach(scroller);
  }, [scroller]);

  /* the list's offset in the scroller, for the list's own arithmetic */
  useLayoutEffect(() => {
    const list = listRef.current;
    if (!scroller || !list) return;
    const measure = () =>
      setMargin(
        list.getBoundingClientRect().top -
          scroller.getBoundingClientRect().top +
          scroller.scrollTop +
          view.current.by,
      );
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(list.parentElement ?? list);
    return () => observer.disconnect();
  }, [scroller]);

  /* the scroller is marked while a subject lands, so its own bars hide with
     the rows: they would show the position being left before the one
     landed on. The bars come and go with the scroller's size, so a style
     rule on the mark hides them, not a style set on the bars */
  useLayoutEffect(() => {
    if (!scroller) return;
    if (landing) scroller.dataset.landing = "";
    else delete scroller.dataset.landing;
  }, [scroller, landing]);

  const opened = useRef<string | null>(null);
  useLayoutEffect(() => {
    if (!scroller || opened.current === subject || entries.length === 0) return;
    opened.current = subject;
    guessWidth.current = 0;
    owner.current!.open(options.opening ?? null);
  });

  /* after every commit the owner puts the view where it belongs, before
     the page is painted: added rows above a held row never show a jump */
  useLayoutEffect(() => {
    owner.current!.reconcile();
  });
  /* rows that just landed may still leave the reader near an end: ask for
     the next stretch while they are */
  useLayoutEffect(() => {
    owner.current!.ahead();
  }, [entries.length]);

  const { sentTurn, anchorSentTurn } = options;
  useLayoutEffect(() => {
    if (sentTurn && keys.has(sentTurn))
      owner.current!.takeSentTurn(sentTurn, anchorSentTurn);
  }, [sentTurn, anchorSentTurn, keys]);

  const toBottom = useCallback(() => owner.current!.toBottom(), []);
  /* a size around the list changed, told from a size observer */
  const settle = useCallback(() => {
    const was = view.current.sizing;
    view.current.sizing = true;
    try {
      owner.current!.reconcile();
    } finally {
      view.current.sizing = was;
    }
  }, []);
  const goTo = useCallback((place: ReaderPlace) => owner.current!.goTo(place), []);
  const place = useCallback(() => owner.current!.place(), []);

  const measureRow = useCallback(
    (node: HTMLDivElement | null) => {
      const index = node ? Number(node.dataset.index) : NaN;
      const entry = Number.isInteger(index)
        ? live.current.options.entries[index]
        : undefined;
      const first =
        entry !== undefined &&
        !virtual.itemSizeCache.has(live.current.options.keyOf(entry));
      virtual.measureElement(node);
      if (!node || entry === undefined) return;
      /* the list skips measuring while the view moves and would place the
         row by its guess, or by a size it had before, until a later frame:
         the reader would see the gap */
      const height = node.getBoundingClientRect().height;
      virtual.resizeItem(index, height);
      if (first) live.current.options.onMeasured?.(entry, height, widthForGuess());
    },
    [virtual],
  );
  /* with no scroller, or one with no height to show anything in, there is
     nothing to window: every row is drawn, in order, at the place its
     guessed height gives it */
  const windowing = Boolean(scroller) && virtual.scrollRect?.height !== 0;
  const items = windowing
    ? virtual.getVirtualItems()
    : entries.reduce<{ index: number; start: number }[]>((all, _entry, index) => {
        const previous = all[index - 1];
        const start = previous
          ? previous.start + estimateOf(entries[index - 1]!, 720)
          : virtual.options.scrollMargin;
        all.push({ index, start });
        return all;
      }, []);
  return {
    items,
    room,
    atBottom,
    toBottom,
    settle,
    goTo,
    place,
    listProps: {
      ref: listRef,
      /* a row drawn taller than the list has placed it yet is clipped to
         the list rather than stretching the scroll range past the content:
         what is pinned to the content's foot would ride up with it */
      style: {
        position: "relative",
        height: virtual.getTotalSize(),
        overflowY: "clip",
        visibility: landing ? "hidden" : undefined,
      },
    } satisfies ComponentProps<"div">,
    attributes(index: number, start: number): ComponentProps<"div"> {
      return {
        ref: measureRow,
        "data-index": index,
        "data-row-key": keyOf(entries[index]!),
        style: {
          position: "absolute",
          top: 0,
          left: 0,
          width: "100%",
          transform: `translateY(${start - virtual.options.scrollMargin}px)`,
        },
      } as ComponentProps<"div">;
    },
  };
}
