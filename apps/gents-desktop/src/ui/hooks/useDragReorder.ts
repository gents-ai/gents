/* Reordering a strip of same-sized items by dragging, shared so every
   reorderable strip feels the same. Pressed, an item lifts and follows the
   pointer after a few pixels while the items it passes slide aside; it
   stays within the slots a move may reach. Let go, it glides into its slot:
   the owner applies the move to the order it draws at once, and an order
   that later goes back animates back. With `animateReorders`, any other
   change of order animates from where the items were. Geometry is taken at
   the press, before anything moved; items carry `data-reorder-key`. */
import {
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent,
} from "react";

const EASE = "cubic-bezier(0.2, 0, 0, 1)";
const SETTLE = `transform 180ms ${EASE}`;
const LIFT = "scale 150ms ease-out, box-shadow 150ms ease-out, opacity 150ms ease-out";
const THRESHOLD = 4;

type Pressed = {
  from: number;
  /** the held item, by key: indexes name items only in one order */
  key: string;
  /** the order the press measured */
  order: string;
  /** where the pointer went down, moved along with the held item's slot */
  start: number;
  /** where the pointer is */
  last: number;
  starts: number[];
  step: number;
  lo: number;
  hi: number;
};

type Drag = { kind: "drag"; from: number; to: number; delta: number; step: number };
type Held = {
  kind: "held";
  from: number;
  to: number;
  step: number;
  /** the order it was dropped against */
  order: string;
  key: string;
  /** how far from its slot the item was let go */
  dropDelta: number;
};

const reducedMotion = () =>
  typeof matchMedia === "function" &&
  matchMedia("(prefers-reduced-motion: reduce)").matches;

export function useDragReorder<E extends HTMLElement = HTMLElement>({
  axis,
  keys,
  canMove,
  commit,
  animateReorders = false,
}: {
  axis: "x" | "y";
  /** the items' keys in their current order */
  keys: readonly string[];
  canMove: (from: number, to: number) => boolean;
  /** applies the move to the order the owner draws, in this call */
  commit: (from: number, to: number) => void;
  animateReorders?: boolean;
}) {
  const container = useRef<E | null>(null);
  const order = keys.join("\n");
  const pressed = useRef<Pressed | null>(null);
  const [phase, setPhase] = useState<Drag | Held | null>(null);

  /* a held move whose order has arrived is drawn settled at once: the new
     places are where the held items already stand */
  const current = phase?.kind === "held" && phase.order !== order ? null : phase;
  const arrived = phase?.kind === "held" && current === null ? phase : null;
  /* a drop the owner did not apply leaves the order as it was: the item
     goes back to its slot rather than waiting in the new one */
  useLayoutEffect(() => {
    if (phase?.kind === "held" && phase.order === order) setPhase(null);
  }, [phase, order]);
  /* whether the held slot was ever painted; a move applied at once never is */
  const heldShown = useRef(false);
  useLayoutEffect(() => {
    heldShown.current = current?.kind === "held";
  }, [current]);

  const translate = (d: number) =>
    axis === "x" ? `translateX(${d}px)` : `translateY(${d}px)`;
  const items = () => [
    ...(container.current?.querySelectorAll<HTMLElement>("[data-reorder-key]") ?? []),
  ];
  const startOf = (el: HTMLElement) => {
    const r = el.getBoundingClientRect();
    return axis === "x" ? r.left : r.top;
  };

  /* where the items rest, in this order, ignoring how far a drag has
     moved them: their own transforms are lifted while they are read */
  const measure = (from: number) => {
    const els = items();
    const kept = els.map((el) => [
      el.style.transition,
      el.style.transform,
      el.style.scale,
    ]);
    for (const el of els) {
      el.style.transition = "none";
      el.style.transform = "none";
      el.style.scale = "none";
    }
    const starts = els.map(startOf);
    const size =
      els[from]?.getBoundingClientRect()[axis === "x" ? "width" : "height"] ?? 0;
    els.forEach((el, i) => {
      [el.style.transition, el.style.transform, el.style.scale] = kept[i]!;
    });
    let lo = from;
    while (lo > 0 && canMove(from, lo - 1)) lo--;
    let hi = from;
    while (hi < starts.length - 1 && canMove(from, hi + 1)) hi++;
    return {
      from,
      starts,
      /* items share one size; a neighbour steps by it plus any gap */
      step:
        starts.length > 1
          ? (starts[starts.length - 1]! - starts[0]!) / (starts.length - 1)
          : size,
      lo,
      hi,
    };
  };

  /* the drag where the pointer last was: the slot it is nearest, within
     the slots a move may reach */
  const dragAt = (p: Pressed): Drag => {
    const at = Math.max(
      p.starts[p.lo]!,
      Math.min(p.starts[p.hi]!, p.starts[p.from]! + (p.last - p.start)),
    );
    let to = p.lo;
    for (let i = p.lo; i <= p.hi; i++)
      if (Math.abs(p.starts[i]! - at) < Math.abs(p.starts[to]! - at)) to = i;
    return {
      kind: "drag",
      from: p.from,
      to,
      delta: at - p.starts[p.from]!,
      step: p.step,
    };
  };

  const seen = useRef(new Map<string, number>());
  useLayoutEffect(() => {
    const base = container.current ? startOf(container.current) : 0;
    const now = items().map((el) => ({
      el,
      key: el.dataset.reorderKey ?? "",
      at: startOf(el) - base,
    }));
    const motion = !reducedMotion();
    if (arrived) {
      const el = now.find((item) => item.key === arrived.key)?.el;
      if (motion && !heldShown.current && arrived.dropDelta && el?.animate)
        el.animate(
          [{ transform: translate(arrived.dropDelta) }, { transform: translate(0) }],
          {
            duration: 180,
            easing: EASE,
          },
        );
      setPhase(null);
    } else if (animateReorders && motion && !phase) {
      for (const { el, key, at } of now) {
        const before = seen.current.get(key);
        if (before !== undefined && before !== at && el.animate)
          el.animate(
            [{ transform: translate(before - at) }, { transform: translate(0) }],
            {
              duration: 180,
              easing: EASE,
            },
          );
      }
    }
    seen.current = new Map(now.map(({ key, at }) => [key, at]));
    // a change of order is what moves items; the rest is read as it stands
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [order]);

  /* the order changed under a held press (another item closed or arrived
     meanwhile): the press's indexes and geometry no longer name the same
     items. The held item is found again by its key and the strip measured
     afresh, the pointer's origin moved by as much as its slot moved, so it
     stays under the pointer and the drag goes on. If it is gone, the drag
     ends with nothing moved. */
  useLayoutEffect(() => {
    const p = pressed.current;
    if (!p || p.order === order) return;
    const from = keys.indexOf(p.key);
    if (from < 0) {
      pressed.current = null;
      delete document.documentElement.dataset.dragging;
      setPhase((now) => (now?.kind === "drag" ? null : now));
      return;
    }
    const measured = measure(from);
    const next: Pressed = {
      ...p,
      ...measured,
      order,
      start: p.start + (measured.starts[from]! - p.starts[p.from]!),
    };
    pressed.current = next;
    setPhase((now) => (now?.kind === "drag" ? dragAt(next) : now));
    // the press is what changes here; the rest is read as it stands
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [order]);

  const release = () => {
    const p = pressed.current;
    if (!p) return;
    pressed.current = null;
    delete document.documentElement.dataset.dragging;
    if (phase?.kind !== "drag" || phase.to === phase.from || p.order !== order) {
      setPhase(null);
      return;
    }
    const held = order;
    setPhase({
      kind: "held",
      from: phase.from,
      to: phase.to,
      step: phase.step,
      order: held,
      key: keys[phase.from] ?? "",
      dropDelta: p.starts[phase.from]! + phase.delta - p.starts[phase.to]!,
    });
    commit(phase.from, phase.to);
  };

  /** pointer handlers for the part an item is dragged by; it captures the
      pointer, so its own handlers see the whole drag */
  const handle = (from: number) => ({
    onPointerDown: (e: PointerEvent<HTMLElement>) => {
      if (e.button !== 0 || phase) return;
      /* no text selection rides along with the drag (WebKit) */
      e.preventDefault();
      getSelection()?.removeAllRanges();
      const at = axis === "x" ? e.clientX : e.clientY;
      pressed.current = {
        ...measure(from),
        key: keys[from] ?? "",
        order,
        start: at,
        last: at,
      };
      document.documentElement.dataset.dragging = "reorder";
      e.currentTarget.setPointerCapture(e.pointerId);
    },
    onPointerMove: (e: PointerEvent<HTMLElement>) => {
      const p = pressed.current;
      if (!p) return;
      p.last = axis === "x" ? e.clientX : e.clientY;
      if (phase?.kind !== "drag" && Math.abs(p.last - p.start) < THRESHOLD) return;
      setPhase(dragAt(p));
    },
    onPointerUp: release,
    onPointerCancel: release,
    onLostPointerCapture: release,
  });

  /** an item's place while something moves */
  const style = (position: number): CSSProperties => {
    if (arrived) return { transition: "none" };
    if (!current) return { transition: `${SETTLE}, ${LIFT}` };
    if (position === current.from)
      return current.kind === "drag"
        ? { transform: translate(current.delta), scale: "1.015", transition: LIFT }
        : {
            transform: translate(current.to === current.from ? 0 : slotOffset(current)),
            opacity: 0.6,
            transition: `${SETTLE}, ${LIFT}`,
          };
    const shift =
      current.from < position && position <= current.to
        ? -current.step
        : current.to <= position && position < current.from
          ? current.step
          : 0;
    return { transform: translate(shift), transition: `${SETTLE}, ${LIFT}` };
  };

  return {
    container,
    handle,
    style,
    /** the item above the others while it moves */
    moving: current?.from ?? null,
    /** the item lifted under the pointer */
    lifted: current?.kind === "drag" ? current.from : null,
    /** whether a drag or a held move is under way */
    active: phase !== null,
  };
}

const slotOffset = (held: Held) => (held.to - held.from) * held.step;
