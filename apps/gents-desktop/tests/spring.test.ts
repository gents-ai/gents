import { afterEach, describe, expect, it, vi } from "vitest";

import { createSpring } from "../src/ui/lib/spring";

function frames(frameMs: number) {
  let now = 0;
  let queued: ((t: number) => void) | null = null;
  vi.stubGlobal("requestAnimationFrame", (callback: (t: number) => void) => {
    queued = callback;
    return 1;
  });
  vi.stubGlobal("cancelAnimationFrame", () => {
    queued = null;
  });
  vi.spyOn(performance, "now").mockImplementation(() => now);
  return {
    run(limit: number) {
      for (let i = 0; i < limit && queued; i += 1) {
        const next = queued;
        queued = null;
        now += frameMs;
        next(now);
      }
    },
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("the divider's spring", () => {
  for (const frameMs of [16, 64, 250]) {
    it(`eases to its target without swinging past it, at ${frameMs} ms a frame`, () => {
      const clock = frames(frameMs);
      let value = 0;
      const seen: number[] = [];
      let rested = false;
      const spring = createSpring({
        get: () => value,
        set: (next) => {
          value = next;
          seen.push(next);
        },
      });
      spring.to(520, 0, () => {
        rested = true;
      });
      clock.run(400);
      expect(rested).toBe(true);
      expect(value).toBe(520);
      expect(Math.max(...seen)).toBeLessThan(521);
      expect(Math.min(...seen)).toBeGreaterThanOrEqual(0);
    });
  }
});
