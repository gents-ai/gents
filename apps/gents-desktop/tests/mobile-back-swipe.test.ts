import { renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { isMobileBackSwipe, useMobileBackSwipe } from "../src/hooks/useMobileBackSwipe";

describe("mobile back swipe", () => {
  it("accepts a deliberate right swipe beginning at the left edge", () => {
    expect(isMobileBackSwipe({ x: 18, y: 240 }, { x: 132, y: 252 })).toBe(true);
  });

  it("rejects gestures away from the edge or dominated by vertical movement", () => {
    expect(isMobileBackSwipe({ x: 80, y: 240 }, { x: 190, y: 244 })).toBe(false);
    expect(isMobileBackSwipe({ x: 18, y: 240 }, { x: 100, y: 340 })).toBe(false);
  });
});

describe("the back swipe across renders", () => {
  it("keeps its listeners while its handler changes, and calls the newest", () => {
    vi.stubGlobal("innerWidth", 390);
    const added = vi.spyOn(document, "addEventListener");
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = renderHook(({ onBack }) => useMobileBackSwipe(true, onBack), {
      initialProps: { onBack: first },
    });
    const listening = added.mock.calls.length;
    rerender({ onBack: second });
    expect(added.mock.calls.length).toBe(listening);

    const at = (x: number) => ({ clientX: x, clientY: 300 });
    document.dispatchEvent(
      Object.assign(new Event("touchstart"), { touches: [at(10)] }),
    );
    document.dispatchEvent(
      Object.assign(new Event("touchend"), { changedTouches: [at(200)] }),
    );
    expect(second).toHaveBeenCalledOnce();
    expect(first).not.toHaveBeenCalled();
    added.mockRestore();
    vi.unstubAllGlobals();
  });
});
