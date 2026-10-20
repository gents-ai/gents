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
    if (stick.current) viewport.scrollTop = viewport.scrollHeight;
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

const settled = new WeakMap<Element, number>();
// Every write this module makes to a transcript's position. WebKit keeps
// the position in whole CSS pixels and drops a write's fraction, which
// leaves a held row up to a pixel below where it was; rounded, it lands
// within half a pixel.
function setScroll(el: HTMLElement, top: number) {
  el.scrollTop = Math.round(top);
  settled.set(el, el.scrollTop);
}

const REPIN_PX = 24;
const INTENT_MS = 300;
const TAIL_MS = 1000;
type PositionCorrection = ReturnType<typeof createPositionCorrection>;
const positionCorrections = new WeakMap<
  HTMLElement,
  {
    subject: string;
    correction: PositionCorrection;
    users: number;
  }
>();

function createPositionCorrection(viewport: HTMLElement, content: HTMLElement | null) {
  let movedAt = -Infinity;
  let touching = false;
  let hidden = 0;
  let timer = 0;
  let disposed = false;
  const listeners = new Set<() => void>();
  const resting = () => !touching && performance.now() - movedAt >= TAIL_MS;
  const schedule = (delay = TAIL_MS) => {
    window.clearTimeout(timer);
    timer = window.setTimeout(unhide, delay);
  };
  const unhide = () => {
    window.clearTimeout(timer);
    timer = 0;
    if (!hidden || !content) return;
    if (!resting() && viewport.scrollTop > 0) {
      if (!touching) schedule(TAIL_MS - (performance.now() - movedAt));
      return;
    }
    const before = viewport.scrollTop;
    content.style.removeProperty("margin-top");
    const target = Math.max(
      0,
      Math.min(before + hidden, viewport.scrollHeight - viewport.clientHeight),
    );
    setScroll(viewport, target);
    // WebKit can drop a position write even after its last motion event.
    if (Math.abs(target - viewport.scrollTop) > 2) {
      content.style.marginTop = `${-hidden}px`;
      schedule();
      return;
    }
    hidden = 0;
    for (const listener of listeners) listener();
  };
  const motion = () => {
    movedAt = performance.now();
  };
  const onScroll = () => {
    motion();
    if (hidden && viewport.scrollTop <= 0) unhide();
  };
  const onTouchStart = () => {
    touching = true;
    motion();
  };
  const onTouchEnd = (event: TouchEvent) => {
    if (event.touches.length) return;
    touching = false;
    motion();
    if (hidden) schedule();
  };
  const onKey = (event: KeyboardEvent) => {
    if (
      NAV_KEYS.has(event.key) &&
      !(event.target as Element | null)?.closest?.(
        'input, textarea, [contenteditable="true"]',
      )
    )
      motion();
  };
  viewport.addEventListener("scroll", onScroll, { passive: true });
  viewport.addEventListener("wheel", motion, { passive: true });
  viewport.addEventListener("touchstart", onTouchStart, { passive: true });
  viewport.addEventListener("touchmove", motion, { passive: true });
  viewport.addEventListener("touchend", onTouchEnd, { passive: true });
  viewport.addEventListener("touchcancel", onTouchEnd, { passive: true });
  viewport.addEventListener("keydown", onKey);
  window.addEventListener("keydown", onKey, true);
  const clear = () => {
    window.clearTimeout(timer);
    timer = 0;
    if (hidden) content?.style.removeProperty("margin-top");
    hidden = 0;
  };
  /* put back now, resting or not: a view about to be taken elsewhere
     starts from where the content is */
  const flush = () => {
    window.clearTimeout(timer);
    timer = 0;
    if (!hidden || !content) return;
    const before = viewport.scrollTop;
    content.style.removeProperty("margin-top");
    setScroll(
      viewport,
      Math.max(
        0,
        Math.min(before + hidden, viewport.scrollHeight - viewport.clientHeight),
      ),
    );
    hidden = 0;
  };
  return {
    get pending() {
      return hidden !== 0;
    },
    listeners,
    clear,
    flush,
    shift(delta: number) {
      if (disposed || Math.abs(delta) < 0.5) return;
      /* no further than the extent allows: hiding content above shrinks
         the extent by as much, which the browser clamps away again */
      if (delta > 0) {
        const room = viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop;
        delta = Math.min(delta, Math.max(0, room));
        if (delta < 0.5) return;
      }
      if (!content || viewport.scrollTop <= 0 || (resting() && !hidden)) {
        setScroll(viewport, viewport.scrollTop + delta);
        return;
      }
      hidden += delta;
      if (hidden) content.style.marginTop = `${-hidden}px`;
      else content.style.removeProperty("margin-top");
      if (!timer) schedule();
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      clear();
      viewport.removeEventListener("scroll", onScroll);
      viewport.removeEventListener("wheel", motion);
      viewport.removeEventListener("touchstart", onTouchStart);
      viewport.removeEventListener("touchmove", motion);
      viewport.removeEventListener("touchend", onTouchEnd);
      viewport.removeEventListener("touchcancel", onTouchEnd);
      viewport.removeEventListener("keydown", onKey);
      window.removeEventListener("keydown", onKey, true);
    },
  };
}

function acquirePositionCorrection(
  viewport: HTMLElement,
  content: HTMLElement | null,
  subject: string,
) {
  let held = positionCorrections.get(viewport);
  if (held && held.subject !== subject) {
    held.correction.dispose();
    held = undefined;
  }
  if (!held) {
    held = {
      subject,
      correction: createPositionCorrection(viewport, content),
      users: 0,
    };
    positionCorrections.set(viewport, held);
  }
  held.users++;
  const owner = held;
  return {
    correction: owner.correction,
    release() {
      if (--owner.users === 0) {
        owner.correction.dispose();
        if (positionCorrections.get(viewport) === owner)
          positionCorrections.delete(viewport);
      }
    },
  };
}

/* the line a reader's eye is on, this far below the scroller's top */
const READING_LINE_PX = 72;
const NAV_KEYS = new Set([
  "ArrowUp",
  "ArrowDown",
  "PageUp",
  "PageDown",
  "Home",
  "End",
  " ",
]);
const UP_KEYS = new Set(["ArrowUp", "PageUp", "Home"]);
const HOLDABLE = "[data-step-key],[data-anchor-key],[data-timeline-key]";
/* a row's key: shared by every row that stands for the same message */
const KEY_ATTRIBUTES = ["data-step-key", "data-anchor-key", "data-timeline-key"];

const insetAt = (el: Element) => {
  const declared = parseFloat(
    getComputedStyle(el).getPropertyValue("--step-scroll-inset"),
  );
  return Number.isFinite(declared) ? declared : 8;
};

const decorative = (node: Node) =>
  (node instanceof Element ? node : node.parentElement)?.closest(
    "[data-slot=ascii-loader]",
  ) != null;

const keyOf = (el: Element) =>
  KEY_ATTRIBUTES.map((name) => el.getAttribute(name)).find((key) => key) ?? null;

const quoted = (value: string) => `"${value.replace(/["\\]/g, "\\$&")}"`;

export function holdRow(el: Element | null) {
  el?.dispatchEvent(new CustomEvent("transcript:hold", { bubbles: true }));
}

export function jumpToRow(el: Element | null) {
  el?.dispatchEvent(new CustomEvent("transcript:jump", { bubbles: true }));
}

const TURN = "[data-turn]";
/* a sent turn settles this far below the inset: a line of what came
   before stays in view */
const TURN_PEEK = 32;
/* the lowest a sent turn's foot may sit, so a tall message leaves its
   reply room below it */
const TURN_FOOT_SHARE = 1 / 3;
const TURN_FOOT_MIN_PX = 240;
/* a tool step after the turn: a turn doing work, not only answering */
const WORK = "[data-step-key]";
/* how long a sent turn takes to glide to its place */
const TURN_GLIDE_MS = 420;
/* the follow's ease toward the foot: this share of the way left each
   frame, never less than this many pixels */
const FOLLOW_EASE = 0.28;
const FOLLOW_MIN_PX = 6;
/* how long after an End key the browser's own scroll for it may still
   arrive (WebKit animates it over a few hundred milliseconds) */
const END_MS = 600;

export function turnFrom(scroller: HTMLElement, way: -1 | 1): Element | null {
  const line = scroller.getBoundingClientRect().top + READING_LINE_PX;
  const turns = Array.from(scroller.querySelectorAll(TURN));

  let at = -1;
  for (let i = 0; i < turns.length; i += 1)
    if (turns[i]!.getBoundingClientRect().top <= line + 0.5) at = i;
  return turns[at + way] ?? null;
}

/**
 * The reader's place in a scroller, held. Two modes, and the reader chooses
 * between them: at the foot, the view follows whatever grows; anywhere else
 * it holds still, keeping the row under the reading line where it is on
 * screen whether the change is above it, below it, or in it.
 *
 * Both are enforced from a ResizeObserver on the content, which runs after
 * layout and before paint, so a correction lands in the frame of the change.
 * The mode changes only on the reader's own input (a wheel, a touch, a key
 * that scrolls, a scrollbar drag): a scroll with none behind it (the
 * browser clamping as content shrinks, WebKit resetting the position while
 * a session loads) says nothing about where they want to be. A row opening
 * or closing announces itself with a `transcript:hold` event first, and is
 * held while it moves.
 *
 * The row is found again by its key when it is drawn anew. The browser's own
 * scroll anchoring is off: WebKit has none, and Chromium's corrected some of
 * the same changes a second time. A new subject starts at its foot.
 */
export function useFollowTail(
  scroller: HTMLElement | null,
  subject: string | null,
  options: {
    /** a turn the person sends settles near the top of the view, with the
        reply filling in beneath it, instead of the view following the foot */
    anchorSentTurn?: boolean;
    /** the key of the turn the person just sent from here, while the app
        still shows its own copy of it: the view goes to that turn once its
        row is the last of the transcript, and once only */
    sentTurn?: string | null;
  } = {},
) {
  const anchorSentTurn = useRef(options.anchorSentTurn ?? false);
  anchorSentTurn.current = options.anchorSentTurn ?? false;
  const offerSent = useRef<((key: string) => void) | null>(null);
  const sentTurn = useRef(options.sentTurn ?? null);
  sentTurn.current = options.sentTurn ?? null;
  /* the last sent turn the view went to, kept across subjects: a session
     opened again offers its last send again */
  const takenSent = useRef<string | null>(null);
  useLayoutEffect(() => {
    if (sentTurn.current) offerSent.current?.(sentTurn.current);
  }, [options.sentTurn]);
  const following = useRef(true);
  const [atBottom, setAtBottom] = useState(true);
  const settleRef = useRef<(() => void) | null>(null);
  const leaving = useRef(false);

  useLayoutEffect(() => {
    following.current = true;
    setAtBottom(true);
    if (!scroller || !subject) return;
    scroller.style.overflowAnchor = "none";
    // No bounce at the ends: WebKit drops a write made during one and stops
    // drawing the scroll area until a later write takes.
    scroller.style.overscrollBehaviorY = "none";
    const position = acquirePositionCorrection(
      scroller,
      scroller.querySelector<HTMLElement>("[data-transcript-content]") ??
        (scroller.firstElementChild as HTMLElement | null),
      subject,
    );
    /* the row held still, by element while it lasts and by key after */
    let anchor: { el: Element; key: string | null; offset: number } | null = null;
    /* whether anything but a held turn's empty room lies past the foot */
    const moreBelow = () =>
      scroller.scrollHeight - emptyRoom() - scroller.clientHeight - scroller.scrollTop >
      4;
    const publishBottom = () => setAtBottom(following.current || !moreBelow());
    const setFollowing = (next: boolean) => {
      following.current = next;
      scroller.dataset.following = String(next);
      publishBottom();
    };
    scroller.dataset.following = "true";
    const top = () => scroller.getBoundingClientRect().top;
    /* The room a sent turn has for its reply: the last turn's block is at
       least this tall, so the turn can sit near the top before the reply
       exists. One number, never sized from the content: at the scroll
       limit every change of it is a clamp. */
    const roomOf = () =>
      parseFloat(scroller.style.getPropertyValue("--turn-room")) || 0;
    const setRoom = (height: number) => {
      const next = Math.max(0, Math.ceil(height));
      if (next === roomOf()) return;
      if (next === 0) scroller.style.removeProperty("--turn-room");
      else scroller.style.setProperty("--turn-room", `${next}px`);
    };
    /* the last turn block, and from it the last turn and row: read from
       the end, as this runs on every streamed chunk */
    const lastBlock = () => {
      const blocks = scroller.querySelectorAll("[data-turn-block]");
      return blocks[blocks.length - 1] ?? null;
    };
    const lastTurn = () => {
      const block = lastBlock();
      const inBlock = block?.querySelector(TURN) ?? null;
      if (inBlock) return inBlock;
      const turns = scroller.querySelectorAll(TURN);
      return turns[turns.length - 1] ?? null;
    };
    /* the row that stands for a turn now, whichever the transcript draws */
    const turnEl = (key: string) =>
      scroller.querySelector(`${TURN}[data-timeline-key=${quoted(key)}]`);
    /* the sent turn the room below is kept for */
    type Room = {
      key: string;
      /** its row as last found; looked up again by key once gone */
      row: Element | null;
      /** the turn before it: a row that left for good, against one
          replaced under another key */
      before: string | null;
      /** whether the reply fit the room at the last layout */
      fit: boolean;
      /** the view height the room was sized for */
      view: number;
      /** where its block and the block before begin, as last laid out:
          known before the commit that takes the turn away is measured */
      blockStart: number;
      blockBeforeStart: number | null;
    };
    let room: Room | null = null;
    /* the turn before a lost room's, while a row under another key may
       still come for the message */
    let lostBefore: string | null = null;
    const roomTurn = () => {
      if (!room) return null;
      if (!room.row?.isConnected || keyOf(room.row) !== room.key)
        room.row = turnEl(room.key);
      return room.row;
    };
    const anchorAt = (el: Element, key = keyOf(el)) => {
      anchor = { el, key, offset: el.getBoundingClientRect().top - top() };
    };
    /* the turn sent from here that the view has yet to go to */
    let sentKey: string | null = null;
    /* where a sent turn's top rests: below the inset and the peek, or
       higher for a tall message, whose foot is kept up in the view */
    const restOf = (turn: Element) => {
      const h = scroller.clientHeight;
      const footLimit = Math.min(h, Math.max(h * TURN_FOOT_SHARE, TURN_FOOT_MIN_PX));
      const rect = turn.getBoundingClientRect();
      return Math.min(insetAt(turn) + TURN_PEEK, footLimit - (rect.bottom - rect.top));
    };
    /* the room a turn at its place needs for its reply: the view below
       that place, less the composer's share of the foot */
    const composerRoom = () =>
      parseFloat(getComputedStyle(scroller).getPropertyValue("--composer-h")) || 0;
    const roomFor = (turn: Element) =>
      Math.max(0, scroller.clientHeight - restOf(turn) - composerRoom());
    /* the foot of the last row: what the reader can scroll down to, the
       empty part of a room aside */
    const lastRowBottom = () => {
      const block = lastBlock();
      if (block) {
        for (let el = block.lastElementChild; el; el = el.previousElementSibling)
          if (el.matches(HOLDABLE)) return el.getBoundingClientRect().bottom;
      }
      const rows = scroller.querySelectorAll(HOLDABLE);
      for (let i = rows.length - 1; i >= 0; i -= 1)
        if (holdable(rows[i]!) === rows[i])
          return rows[i]!.getBoundingClientRect().bottom;
      return null;
    };
    /* how much of the held turn's room its reply has yet to fill */
    const emptyRoom = () => {
      const turn = roomTurn();
      if (!turn) return 0;
      const bottom = lastRowBottom();
      if (bottom === null) return 0;
      return Math.max(0, roomOf() - (bottom - turn.getBoundingClientRect().top));
    };
    /* the turn from this sent row on is doing work: a tool step follows it */
    const turnHasWork = (turn: Element) => {
      for (let row = turn.nextElementSibling; row; row = row.nextElementSibling)
        if (row.matches(WORK) || row.querySelector(WORK)) return true;
      return false;
    };
    const reducedMotion = () =>
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const footOf = () => scroller.scrollHeight - scroller.clientHeight;
    /* the ease toward the foot while following; content that shrank, a
       move of more than half a view, a reader whose scroll is recent, and
       reduced motion are written at once */
    let followFrame = 0;
    const stopFollowGlide = () => {
      if (!followFrame) return;
      cancelAnimationFrame(followFrame);
      followFrame = 0;
    };
    const stepFollow = () => {
      followFrame = 0;
      if (!following.current) return;
      const delta = footOf() - scroller.scrollTop;
      if (delta < 0.5) {
        if (delta < 0 || delta >= 0.5) setScroll(scroller, footOf());
        return;
      }
      const move = Math.min(delta, Math.max(delta * FOLLOW_EASE, FOLLOW_MIN_PX));
      setScroll(scroller, scroller.scrollTop + move);
      if (footOf() - scroller.scrollTop >= 0.5)
        followFrame = requestAnimationFrame(stepFollow);
    };
    /* the foot as last seen: growth since is eased; any shortfall beyond
       it is a clamp under the reader, put back at once */
    let lastFoot = footOf();
    const pin = () => {
      position.correction.clear();
      const foot = footOf();
      const growth = Math.max(0, foot - lastFoot);
      lastFoot = foot;
      const delta = foot - scroller.scrollTop;
      if (Math.abs(delta) < 0.5) return;
      const displaced = delta - growth;
      if (delta < 0 || displaced >= 0.5)
        setScroll(scroller, scroller.scrollTop + displaced);
      if (foot - scroller.scrollTop < 0.5) return;
      const now = performance.now();
      const readerRecent = now <= intentUntil || now - tailAt < TAIL_MS;
      if (growth > scroller.clientHeight / 2 || readerRecent || reducedMotion()) {
        stopFollowGlide();
        setScroll(scroller, foot);
        return;
      }
      if (!followFrame) followFrame = requestAnimationFrame(stepFollow);
    };
    const holdable = (el: Element | null): Element | null => {
      let at = el?.closest(HOLDABLE) ?? null;
      while (at && at.closest("[data-slot=scroll-area-viewport]") !== scroller)
        at = at.parentElement?.closest(HOLDABLE) ?? null;
      return at;
    };
    const capture = () => {
      const box = scroller.getBoundingClientRect();
      const line = box.top + READING_LINE_PX;
      let el = holdable(
        document.elementFromPoint?.(box.left + box.width / 2, line) ?? null,
      );
      /* the line falls in a gap: the first row whose foot is below it,
         by halving over the rows, which stack in document order */
      if (!el) {
        const rows = Array.from(scroller.querySelectorAll(HOLDABLE)).filter(
          (row) => holdable(row) === row,
        );
        let lo = 0;
        let hi = rows.length;
        while (lo < hi) {
          const mid = (lo + hi) >> 1;
          if (rows[mid]!.getBoundingClientRect().bottom > line) hi = mid;
          else lo = mid + 1;
        }
        el = rows[lo] ?? null;
      }
      anchor = el
        ? { el, key: keyOf(el), offset: el.getBoundingClientRect().top - box.top }
        : null;
      settled.set(scroller, scroller.scrollTop);
    };
    const hold = () => {
      if (!anchor) return capture();
      let el: Element | null = anchor.el;
      if (!el.isConnected && anchor.key) {
        const key = quoted(anchor.key);
        el = scroller.querySelector(
          KEY_ATTRIBUTES.map((name) => `[${name}=${key}]`).join(","),
        );
      }
      if (!el) return capture();
      anchor.el = el;
      const since = readerMoved();
      if (since !== 0) {
        anchor.offset -= since;
        settled.set(scroller, scroller.scrollTop);
      }
      const delta = el.getBoundingClientRect().top - top() - anchor.offset;
      position.correction.shift(delta);
    };
    let intentUntil = 0;
    let tailAt = -Infinity;
    let tailWay = 0;
    const readerMoved = () => {
      const since = scroller.scrollTop - (settled.get(scroller) ?? scroller.scrollTop);
      const way = Math.sign(since);
      const now = performance.now();
      if (
        way === 0 ||
        (now > intentUntil &&
          (now - tailAt > TAIL_MS ||
            way !== tailWay ||
            (way < 0 && distanceFromFoot(scroller) < 0.5)))
      )
        return 0;
      intend();
      tailAt = now;
      tailWay = way;
      return since;
    };
    leaving.current = false;
    let dragging = false;
    /* the view's height as last laid out */
    let viewHeight = 0;
    const onRoomTurn = () =>
      room !== null && anchor?.key === room.key && !following.current;
    const measureBlocks = (turn: Element) => {
      if (!room) return;
      const block = turn.closest("[data-turn-block]");
      const before = block?.previousElementSibling;
      room.blockStart = (block?.getBoundingClientRect().top ?? 0) + scroller.scrollTop;
      room.blockBeforeStart =
        before?.matches("[data-turn-block]") === true
          ? before.getBoundingClientRect().top + scroller.scrollTop
          : null;
    };
    const turnBefore = (turn: Element) => {
      const from = turn.closest("[data-turn-block]") ?? turn;
      for (let el = from.previousElementSibling; el; el = el.previousElementSibling) {
        if (el.matches(TURN)) return el;
        const inner = el.querySelector(TURN);
        if (inner) return inner;
      }
      return null;
    };
    const giveRoom = (turn: Element, key: string) => {
      const before = turnBefore(turn);
      room = {
        key,
        row: turn,
        before: before ? keyOf(before) : null,
        fit: true,
        view: scroller.clientHeight,
        blockStart: 0,
        blockBeforeStart: null,
      };
      lostBefore = null;
      setRoom(roomFor(turn));
      measureBlocks(turn);
    };
    /* the sent turn has no row any more: a reader on it keeps the view
       where it is, held by the row on the reading line */
    const dropRoom = () => {
      const wasOnTurn = onRoomTurn();
      room = null;
      if (wasOnTurn) {
        anchor = null;
        capture();
      }
    };
    /* The sent row is gone. Drawn again under another key (the runtime
       names the request anew), in the same commit or later, the hold and
       the room move to it. Until then the room stays with the block
       before: taking it away shrinks the content under a reader at its
       end, which the browser clamps as a jump. */
    const lastTurnAfter = (before: string | null) => {
      const next = lastTurn();
      const key = next ? keyOf(next) : null;
      return next && key && key !== before ? { next, key } : null;
    };
    const adopt = ({ next, key }: { next: Element; key: string }, held: boolean) => {
      giveRoom(next, key);
      takenSent.current = key;
      if (held) anchorAt(next, key);
    };
    const dropLostTurn = () => {
      if (room && !roomTurn()) {
        /* the block before takes the room, larger by the lost block's
           start less its own, before anything is measured: the extent
           stays, so the browser clamps nothing */
        if (room.blockBeforeStart !== null)
          setRoom(roomOf() + room.blockStart - room.blockBeforeStart);
        const found = lastTurnAfter(room.before);
        if (found) {
          adopt(found, onRoomTurn() || (!following.current && anchor === null));
          return;
        }
        lostBefore = room.before ?? "";
        dropRoom();
        return;
      }
      if (!room && lostBefore !== null) {
        const found = lastTurnAfter(lostBefore);
        if (found) adopt(found, !following.current);
      }
    };
    /* The held turn after a change. The room follows the view's size and
       grows as the composer shrinks; it never shrinks for the composer,
       which would clamp a reader at the end. A turn doing work is followed
       from the layout where its reply crosses the room's end; a plain
       answer stays held however long it grows. */
    const settleTurn = () => {
      if (!room) return;
      const turn = roomTurn();
      if (!turn) {
        dropRoom();
        return;
      }
      const want = roomFor(turn);
      if (scroller.clientHeight !== room.view) {
        room.view = scroller.clientHeight;
        setRoom(want);
      } else if (want > roomOf()) setRoom(want);
      measureBlocks(turn);
      const bottom = lastRowBottom();
      const over =
        bottom === null ? 0 : bottom - turn.getBoundingClientRect().top - roomOf();
      const crossing = over > 0 && room.fit;
      room.fit = over <= 0;
      if (crossing && !gliding && onRoomTurn() && turnHasWork(turn)) {
        anchor = null;
        setFollowing(true);
        pin();
      }
    };
    /* the glide of a sent turn to its place; ended by the reader's input,
       a jump, and the way back */
    let glide = 0;
    /* from the glide's first frame to its last */
    let gliding = false;
    const stopGlide = () => {
      gliding = false;
      if (!glide) return;
      cancelAnimationFrame(glide);
      glide = 0;
    };
    const anchorTurn = (turn: Element, key: string) => {
      const rest = restOf(turn);
      giveRoom(turn, key);
      /* a correction still hidden would land on the glide as one jump */
      position.correction.flush();
      let from = turn.getBoundingClientRect().top - top();
      setFollowing(false);
      leaving.current = false;
      stopGlide();
      /* from further than a view away: put within a view at once, glide
         the rest */
      const reach = scroller.clientHeight;
      if (Math.abs(from - rest) > reach) {
        const skip = from - rest - Math.sign(from - rest) * reach;
        setScroll(scroller, scroller.scrollTop + skip);
        from = turn.getBoundingClientRect().top - top();
      }
      const delta = from - rest;
      /* the hold keeps the row where the glide has it so far */
      anchor = { el: turn, key, offset: from };
      if (reducedMotion() || Math.abs(delta) < 24) {
        anchor.offset = rest;
        position.correction.shift(delta);
        publishBottom();
        return;
      }
      const started = performance.now();
      gliding = true;
      const step = () => {
        glide = 0;
        if (!anchor || anchor.key !== key || following.current) {
          gliding = false;
          return;
        }
        /* one replaced mid-glide is glided on from where its copy is */
        const row = turnEl(key);
        if (!row) return;
        const p = Math.min(1, (performance.now() - started) / TURN_GLIDE_MS);
        const eased = 1 - (1 - p) ** 3;
        const offset = from - delta * eased;
        anchor.offset = offset;
        const at = row.getBoundingClientRect().top - top();
        if (Math.abs(at - offset) >= 0.5)
          setScroll(scroller, scroller.scrollTop + (at - offset));
        if (p < 1) glide = requestAnimationFrame(step);
        else {
          gliding = false;
          publishBottom();
        }
      };
      glide = requestAnimationFrame(step);
    };

    /* the turn sent from here, once its row is the last: held near the
       top with room below, or followed, by the preference; not while the
       reader drags the scrollbar */
    const takeSentTurn = () => {
      if (!sentKey || dragging) return false;
      const key = sentKey;
      const turn = turnEl(key);
      if (!turn || turn !== lastTurn()) return false;
      takenSent.current = key;
      sentKey = null;
      stopGlide();
      if (!anchorSentTurn.current) {
        anchor = null;
        leaving.current = false;
        setFollowing(true);
        pin();
        return true;
      }
      /* a view's height of room before anything is measured: the commit
         may also have taken rows away below, and the first layout would
         clamp a view at its foot before the exact room is given */
      if (viewHeight) setRoom(viewHeight);
      anchorTurn(turn, key);
      hold();
      return true;
    };
    offerSent.current = (key) => {
      if (key === takenSent.current) return;
      sentKey = key;
      takeSentTurn();
    };
    const reconcile = () => {
      if (dragging) return;
      dropLostTurn();
      settleTurn();
      if (following.current) pin();
      else hold();
      publishBottom();
      viewHeight = scroller.clientHeight;
    };
    settleRef.current = reconcile;
    pin();
    viewHeight = scroller.clientHeight;
    /* a send known as the view mounts (a new session's first message):
       taken from the foot, after the view has been put there */
    if (sentTurn.current) offerSent.current(sentTurn.current);

    const sizes = new ResizeObserver(() => reconcile());
    const watch = () => {
      sizes.disconnect();
      for (const child of Array.from(scroller.children)) sizes.observe(child);
    };
    watch();
    const changes = new MutationObserver((records) => {
      const content = records.filter((record) => !decorative(record.target));
      if (content.length === 0) return;
      if (content.some((record) => record.target === scroller)) watch();
      if (content.some((record) => record.addedNodes.length > 0) && takeSentTurn())
        return;
      reconcile();
    });
    changes.observe(scroller, { childList: true, subtree: true, characterData: true });

    const intend = () => {
      intentUntil = performance.now() + INTENT_MS;
      stopGlide();
    };
    const release = () => {
      stopFollowGlide();
      leaving.current = true;
      if (following.current) {
        anchor = null;
        setFollowing(false);
      }
    };
    /* until when the browser's own scroll for an End key may still be
       arriving: each of its events is a move to the foot, whatever position
       its animation passes through, not a reader leaving it. Any other
       input ends it first, as its event comes before the scroll it makes. */
    let endingUntil = 0;
    const onWheel = (event: WheelEvent) => {
      endingUntil = 0;
      intend();
      if (event.deltaY < 0) release();
      else if (event.deltaY > 0) leaving.current = false;
    };
    let touchY: number | undefined;
    const onTouchStart = (event: TouchEvent) => {
      touchY = event.touches[0]?.clientY;
    };
    const onTouchMove = (event: TouchEvent) => {
      endingUntil = 0;
      intend();
      const y = event.touches[0]?.clientY;
      if (touchY !== undefined && y !== undefined && y > touchY) release();
      else if (touchY !== undefined && y !== undefined && y < touchY)
        leaving.current = false;
      touchY = y;
    };
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as Element | null;
      if (target?.closest?.('input, textarea, [contenteditable="true"]')) return;
      if (!NAV_KEYS.has(event.key)) return;
      intend();
      if (UP_KEYS.has(event.key)) release();
      else leaving.current = false;
      /* End asks for the end, whatever the browser's own scroll lands on:
         its animation aims at the foot as it was at the key, and a reply
         still streaming has moved the foot on by the time it lands, which
         left the reader short of it and not following */
      endingUntil = event.key === "End" ? performance.now() + END_MS : 0;
      if (endingUntil) {
        stopGlide();
        anchor = null;
        setFollowing(true);
        pin();
      }
    };
    const area = scroller.parentElement;
    const onPointerDown = (event: PointerEvent) => {
      if (
        (event.target as Element | null)?.closest?.("[data-slot=scroll-area-scrollbar]")
      ) {
        /* a drag has no direction to read: where it lets go decides */
        endingUntil = 0;
        dragging = true;
        leaving.current = false;
      }
    };
    const onPointerUp = () => {
      if (dragging) intend();
      dragging = false;
    };
    const onScroll = () => {
      readerMoved();
      if (endingUntil && performance.now() <= endingUntil) {
        setFollowing(true);
        pin();
        return;
      }
      endingUntil = 0;
      if (!dragging && performance.now() > intentUntil) return;
      const nearFoot = !leaving.current && distanceFromFoot(scroller) <= REPIN_PX;
      /* while a reply has yet to fill its room, the foot is the turn's
         held place: a reader coming back down is held there again */
      const turn = roomTurn();
      if (nearFoot && turn && room && emptyRoom() > 0) {
        setFollowing(false);
        anchorAt(turn, room.key);
        room.fit = true;
        settled.set(scroller, scroller.scrollTop);
        return;
      }
      setFollowing(nearFoot);
      if (!following.current) capture();
      publishBottom();
    };
    const onHold = (event: Event) => {
      const target = event.target instanceof Element ? event.target : null;
      const el = holdable(target) ?? target;
      if (!el) return;
      setFollowing(false);
      const inset = insetAt(el);
      let offset = el.getBoundingClientRect().top - top();
      if (offset < inset) {
        position.correction.shift(-(inset - offset));
        offset = inset;
      }
      anchor = { el, key: keyOf(el), offset };
      settled.set(scroller, scroller.scrollTop);
    };
    // Chromium can apply a passive wheel or touch scroll on its compositor
    // before dispatching the event; non-passive forces the dispatch first.
    // Neither listener may call preventDefault: that would stop scrolling.
    scroller.addEventListener("wheel", onWheel, { passive: false });
    scroller.addEventListener("touchstart", onTouchStart, { passive: true });
    scroller.addEventListener("touchmove", onTouchMove, { passive: false });
    window.addEventListener("keydown", onKey);
    area?.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("pointerup", onPointerUp);

    let jumpTimer = 0;
    let jumpTo: Element | null = null;
    const jump = () => {
      window.clearTimeout(jumpTimer);
      jumpTimer = 0;
      const el = jumpTo;
      if (!el?.isConnected) return;
      if (performance.now() - tailAt < TAIL_MS) {
        jumpTimer = window.setTimeout(jump, TAIL_MS);
        return;
      }
      jumpTo = null;
      setFollowing(false);
      const inset = insetAt(el);
      setScroll(
        scroller,
        scroller.scrollTop + (el.getBoundingClientRect().top - top() - inset),
      );
      anchorAt(el);
      publishBottom();
    };
    const onJump = (event: Event) => {
      const target = event.target instanceof Element ? event.target : null;
      const el = holdable(target) ?? target;
      if (!el) return;
      stopGlide();
      jumpTo = el;
      leaving.current = false;
      jump();
    };
    scroller.addEventListener("scroll", onScroll, { passive: true });
    scroller.addEventListener("transcript:hold", onHold);
    scroller.addEventListener("transcript:jump", onJump);
    return () => {
      stopGlide();
      stopFollowGlide();
      offerSent.current = null;
      room = null;
      lostBefore = null;
      sentKey = null;
      setRoom(0);
      window.clearTimeout(jumpTimer);
      scroller.removeEventListener("transcript:jump", onJump);
      settleRef.current = null;
      position.release();
      sizes.disconnect();
      changes.disconnect();
      scroller.removeEventListener("wheel", onWheel);
      scroller.removeEventListener("touchstart", onTouchStart);
      scroller.removeEventListener("touchmove", onTouchMove);
      window.removeEventListener("keydown", onKey);
      area?.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("pointerup", onPointerUp);
      scroller.removeEventListener("scroll", onScroll);
      scroller.removeEventListener("transcript:hold", onHold);
    };
  }, [scroller, subject]);

  const toBottom = useCallback(() => {
    if (!scroller) return;
    positionCorrections.get(scroller)?.correction.clear();
    following.current = true;
    leaving.current = false;
    scroller.dataset.following = "true";
    setAtBottom(true);
    setScroll(scroller, scroller.scrollHeight - scroller.clientHeight);
  }, [scroller]);

  const settle = useCallback(() => settleRef.current?.(), []);

  return { atBottom, toBottom, settle };
}

// An older page is asked for while the reader is this many views from the
// top, so it lands above them before they reach it; a page read takes a
// bridge round trip. Never less than OLDER_AHEAD_PX, for a pane only a few
// lines tall. Moving up this recently, the next page follows the one that
// landed.
// Rows that land mid-scroll are kept out of sight above the content's top
// (one write to the position once the view has rested or reached the top;
// a write while WebKit's own scrolling is under way stops it and one during
// fling momentum is dropped). Resting waits out TAIL_MS with no scroll, wheel
// or key event and no finger on a touchscreen, because WebKitGTK's momentum
// goes on sending wheel events and reports `scrollend` before it ends.
const OLDER_AHEAD_VIEWS = 3;
const OLDER_AHEAD_PX = 160;
const OLDER_INTENT_MS = 1000;

type Hold = {
  row: HTMLElement | undefined;
  top: number | undefined;
  scrollTop: number;
  height: number;
  oldestKey: string | null;
  landed: boolean;
  settled: boolean;
};

function shiftOf(viewport: HTMLElement, hold: Hold) {
  if (hold.row?.isConnected && hold.top !== undefined)
    return (
      hold.row.getBoundingClientRect().top -
      hold.top +
      viewport.scrollTop -
      hold.scrollTop
    );
  return viewport.scrollHeight - hold.height;
}

// Older pages load when the reader moves up near the top, keeping the row
// under them in place; the row is put back in a layout effect on the commit
// where `oldestKey` changes, before paint.
export function useOlderPages(
  scroller: HTMLElement | null,
  content: HTMLElement | null,
  subject: string | null,
  hasOlder: boolean,
  load: () => Promise<boolean>,
  oldestKey: string | null,
) {
  const latest = useRef({ hasOlder, load, oldestKey });
  latest.current = { hasOlder, load, oldestKey };
  const [loading, setLoading] = useState(false);
  const [settles, setSettles] = useState(0);
  const hold = useRef<Hold | null>(null);
  const again = useRef<() => void>(() => {});
  const land = useRef<(shift: number) => void>(() => {});

  useLayoutEffect(() => {
    const held = hold.current;
    if (!held || !scroller) return;
    if (!held.landed && oldestKey !== held.oldestKey) {
      land.current(shiftOf(scroller, held));
      held.landed = true;
    }
    if (held.settled) {
      hold.current = null;
      if (held.landed && scroller.scrollHeight > held.height)
        queueMicrotask(() => again.current());
    }
  }, [oldestKey, scroller, settles]);

  useLayoutEffect(() => {
    const viewport = scroller;
    if (!viewport || !subject) return;
    let disposed = false;
    let lastTop = viewport.scrollTop;
    let movedUpAt = -Infinity;
    const position = acquirePositionCorrection(viewport, content, subject);
    const correction = position.correction;
    const onSettled = () => again.current();
    correction.listeners.add(onSettled);
    land.current = (shift) => correction.shift(shift);
    hold.current = null;
    setLoading(false);
    const fetchOlder = async () => {
      if (
        disposed ||
        hold.current ||
        correction.pending ||
        !latest.current.hasOlder ||
        viewport.scrollTop >
          Math.max(OLDER_AHEAD_PX, OLDER_AHEAD_VIEWS * viewport.clientHeight)
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
      } catch {}
      if (disposed || hold.current !== held) return;
      held.settled = true;
      setLoading(false);
      setSettles((count) => count + 1);
    };
    const up = () => {
      movedUpAt = performance.now();
      void fetchOlder();
    };
    again.current = () => {
      if (performance.now() - movedUpAt < OLDER_INTENT_MS) void fetchOlder();
    };
    const onScroll = () => {
      const upward = viewport.scrollTop < lastTop;
      lastTop = viewport.scrollTop;
      if (upward) up();
    };
    const onWheel = (event: WheelEvent) => {
      if (event.deltaY < 0) up();
    };
    let touchY: number | undefined;
    const onTouchStart = (event: TouchEvent) => {
      touchY = event.touches[0]?.clientY;
    };
    const onTouchMove = (event: TouchEvent) => {
      const nextY = event.touches[0]?.clientY;
      if (touchY !== undefined && nextY !== undefined && nextY > touchY) up();
      touchY = nextY;
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (["ArrowUp", "PageUp", "Home"].includes(event.key)) up();
    };
    viewport.addEventListener("scroll", onScroll, { passive: true });
    viewport.addEventListener("wheel", onWheel, { passive: true });
    viewport.addEventListener("touchstart", onTouchStart, { passive: true });
    viewport.addEventListener("touchmove", onTouchMove, { passive: true });
    viewport.addEventListener("keydown", onKeyDown);
    return () => {
      disposed = true;
      again.current = () => {};
      land.current = () => {};
      correction.listeners.delete(onSettled);
      position.release();
      hold.current = null;
      viewport.removeEventListener("scroll", onScroll);
      viewport.removeEventListener("wheel", onWheel);
      viewport.removeEventListener("touchstart", onTouchStart);
      viewport.removeEventListener("touchmove", onTouchMove);
      viewport.removeEventListener("keydown", onKeyDown);
    };
  }, [scroller, content, subject]);
  return loading;
}
