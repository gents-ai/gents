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

  it("retains the closing lease when outside dismissal precedes replacement open", () => {
    vi.useFakeTimers();
    const { result, unmount } = renderHook(() => {
      const first = useExclusivePopover();
      const second = useExclusivePopover();
      return { first, second };
    });

    act(() => result.current.first.onOpenChange(true));
    expect(result.current.first.open).toBe(true);

    act(() => {
      result.current.first.onOpenChange(false);
      result.current.second.onOpenChange(true);
    });
    expect(result.current.first.open).toBe(false);
    expect(result.current.second.open).toBe(false);

    act(() => vi.advanceTimersByTime(149));
    expect(result.current.second.open).toBe(false);
    act(() => vi.advanceTimersByTime(1));
    expect(result.current.second.open).toBe(true);
    unmount();
    act(() => vi.runOnlyPendingTimers());
  });

  it("keeps a reopened owner active after its obsolete release deadline", () => {
    vi.useFakeTimers();
    const { result, unmount } = renderHook(() => {
      const first = useExclusivePopover();
      const second = useExclusivePopover();
      return { first, second };
    });

    act(() => result.current.first.onOpenChange(true));
    act(() => result.current.first.onOpenChange(false));
    act(() => result.current.first.onOpenChange(true));
    act(() => vi.advanceTimersByTime(150));
    expect(result.current.first.open).toBe(true);

    act(() => result.current.second.onOpenChange(true));
    expect(result.current.first.open).toBe(false);
    expect(result.current.second.open).toBe(false);
    act(() => vi.advanceTimersByTime(150));
    expect(result.current.second.open).toBe(true);
    unmount();
  });
});
