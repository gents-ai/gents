/* A spring on one number, stepped by requestAnimationFrame. The one source
   of motion for the divider: a drag's release, a keyboard step and the menu's
   open and close all settle through it, so every move of the divider feels
   the same. Seeded with the gesture's velocity, a flick lands quickly and a
   slow release eases in; with reduced motion asked for it lands at once. */
export type Spring = {
  /** start toward `target` from the current value, with an initial velocity in units per second */
  to: (target: number, velocity?: number, onRest?: () => void) => void;
  /** stop where it is */
  stop: () => void;
  /** whether a settle is under way */
  running: () => boolean;
};

/* seconds: short enough that damping × step stays far below 2 at any
   stiffness the divider uses */
const SUB_STEP = 1 / 240;

export function createSpring({
  get,
  set,
  stiffness = 380,
  /* ratio of critical: just under 1 settles fast with no visible overshoot */
  dampingRatio = 0.95,
}: {
  get: () => number;
  set: (value: number) => void;
  stiffness?: number;
  dampingRatio?: number;
}): Spring {
  let raf: number | null = null;
  const damping = 2 * Math.sqrt(stiffness) * dampingRatio;
  const reduced = () =>
    typeof matchMedia === "function" &&
    matchMedia("(prefers-reduced-motion: reduce)").matches;
  const stop = () => {
    if (raf !== null) cancelAnimationFrame(raf);
    raf = null;
  };
  return {
    running: () => raf !== null,
    stop,
    to(target, velocity = 0, onRest) {
      stop();
      if (reduced()) {
        set(target);
        onRest?.();
        return;
      }
      let x = get();
      let v = velocity;
      let last = performance.now();
      const step = (now: number) => {
        /* A frame's time is integrated in short fixed steps. One explicit
           step as long as a slow frame overcorrects: once damping × dt passes
           2, the velocity flips and grows every frame. A long transcript
           re-laid out under the moving divider makes frames of 200 ms and
           more, and the dock swung wider and wider until it filled the
           window. Bounded, so a hidden tab's frame is still only a moment. */
        let left = Math.min(0.25, (now - last) / 1000);
        last = now;
        while (left > 0) {
          const dt = Math.min(SUB_STEP, left);
          left -= dt;
          const a = -stiffness * (x - target) - damping * v;
          v += a * dt;
          x += v * dt;
        }
        if (Math.abs(v) < 2 && Math.abs(x - target) < 0.5) {
          raf = null;
          set(target);
          onRest?.();
          return;
        }
        set(x);
        raf = requestAnimationFrame(step);
      };
      raf = requestAnimationFrame(step);
    },
  };
}
