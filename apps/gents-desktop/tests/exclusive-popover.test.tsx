import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useExclusivePopover } from "../src/ui/hooks/useExclusivePopover";

describe("exclusive shell popovers", () => {
  afterEach(() => vi.useRealTimers());

  it("closes the current popup before mounting its replacement", () => {
    vi.useFakeTimers();
    const { result, unmount } = renderHook(() => {
      const first = useExclusivePopover();
      const second = useExclusivePopover();
      return { first, second };
    });

    act(() => result.current.first.onOpenChange(true));
    expect(result.current.first.open).toBe(true);

    act(() => result.current.second.onOpenChange(true));
    expect(result.current.first.open).toBe(false);
    expect(result.current.second.open).toBe(false);

    act(() => vi.advanceTimersByTime(149));
    expect(result.current.second.open).toBe(false);
    act(() => vi.advanceTimersByTime(1));
    expect(result.current.second.open).toBe(true);
    unmount();
  });
});
