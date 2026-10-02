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
        /* a long frame (tab hidden) must not launch the value */
        const dt = Math.min(0.064, (now - last) / 1000);
        last = now;
        const a = -stiffness * (x - target) - damping * v;
        v += a * dt;
        x += v * dt;
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
