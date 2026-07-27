import { describe, expect, it } from "vitest";
import { countCoalescedRefreshes } from "./store.js";

describe("countCoalescedRefreshes", () => {
  it("coalesces a burst under the debounce window to one refresh", () => {
    expect(countCoalescedRefreshes(10, 50, 5)).toBe(1);
  });

  it("counts spaced events separately when interval exceeds debounce", () => {
    expect(countCoalescedRefreshes(3, 50, 100)).toBe(3);
  });
});
