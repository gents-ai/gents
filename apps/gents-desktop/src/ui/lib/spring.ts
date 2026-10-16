export type Spring = {
  to: (target: number, velocity?: number, onRest?: () => void) => void;
  stop: () => void;
  running: () => boolean;
};

// Seconds: short enough that damping × step stays far below 2 at any
// stiffness the divider uses.
const SUB_STEP = 1 / 240;

export function createSpring({
  get,
  set,
  stiffness = 380,
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
        // A frame's time is integrated in short fixed steps: once damping ×
        // dt passes 2, the velocity flips and grows every frame (slow frames
        // from long re-layouts under the moving divider), so dt is bounded.
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
