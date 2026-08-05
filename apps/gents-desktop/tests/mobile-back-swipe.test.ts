import { describe, expect, it } from "vitest";

import { isMobileBackSwipe } from "../src/hooks/useMobileBackSwipe";

describe("mobile back swipe", () => {
  it("accepts a deliberate right swipe beginning at the left edge", () => {
    expect(isMobileBackSwipe({ x: 18, y: 240 }, { x: 132, y: 252 })).toBe(true);
  });

  it("rejects gestures away from the edge or dominated by vertical movement", () => {
    expect(isMobileBackSwipe({ x: 80, y: 240 }, { x: 190, y: 244 })).toBe(false);
    expect(isMobileBackSwipe({ x: 18, y: 240 }, { x: 100, y: 340 })).toBe(false);
  });
});
