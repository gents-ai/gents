import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useExclusivePopover } from "../src/ui/hooks/useExclusivePopover";

describe("exclusive shell popovers", () => {
  it("ignores a captured open handler after its owner unmounts", () => {
    const first = renderHook(() => useExclusivePopover());
    const staleOpen = first.result.current.onOpenChange;
    first.unmount();
    const second = renderHook(() => useExclusivePopover());
    act(() => second.result.current.onOpenChange(true));
    second.result.current.popupRef.current = document.createElement("div");
    act(() => staleOpen(true));
    expect(second.result.current.open).toBe(true);
    second.unmount();
  });

  it("closes the current popup before mounting its replacement", () => {
    const { result, unmount } = renderHook(() => {
      const first = useExclusivePopover();
      const second = useExclusivePopover();
      return { first, second };
    });

    act(() => result.current.first.onOpenChange(true));
    expect(result.current.first.open).toBe(true);
    result.current.first.popupRef.current = document.createElement("div");

    act(() => result.current.second.onOpenChange(true));
    expect(result.current.first.open).toBe(false);
    expect(result.current.second.open).toBe(false);

    act(() => result.current.first.onOpenChangeComplete(false));
    expect(result.current.second.open).toBe(true);
    unmount();
  });

  it("retains the closing lease when outside dismissal precedes replacement open", () => {
    const { result, unmount } = renderHook(() => {
      const first = useExclusivePopover();
      const second = useExclusivePopover();
      return { first, second };
    });

    act(() => result.current.first.onOpenChange(true));
    expect(result.current.first.open).toBe(true);
    result.current.first.popupRef.current = document.createElement("div");

    act(() => {
      result.current.first.onOpenChange(false);
      result.current.second.onOpenChange(true);
    });
    expect(result.current.first.open).toBe(false);
    expect(result.current.second.open).toBe(false);

    act(() => result.current.first.onOpenChangeComplete(false));
    expect(result.current.second.open).toBe(true);
    unmount();
  });

  it("hands off immediately when the prior owner never mounted", () => {
    const { result, unmount } = renderHook(() => {
      const first = useExclusivePopover();
      const second = useExclusivePopover();
      return { first, second };
    });

    act(() => {
      result.current.first.onOpenChange(true);
      result.current.second.onOpenChange(true);
    });
    expect(result.current.first.open).toBe(false);
    expect(result.current.second.open).toBe(true);
    unmount();
  });

  it("cancels a pending owner and lets the latest request win", () => {
    const { result, unmount } = renderHook(() => {
      const first = useExclusivePopover();
      const second = useExclusivePopover();
      const third = useExclusivePopover();
      return { first, second, third };
    });

    act(() => result.current.first.onOpenChange(true));
    result.current.first.popupRef.current = document.createElement("div");
    act(() => result.current.second.onOpenChange(true));
    act(() => result.current.second.onOpenChange(false));
    act(() => result.current.third.onOpenChange(true));
    act(() => result.current.first.onOpenChangeComplete(false));
    expect(result.current.second.open).toBe(false);
    expect(result.current.third.open).toBe(true);
    unmount();
  });

  it("does not release a same-owner reopen on a stale close completion", () => {
    const { result, unmount } = renderHook(() => {
      const first = useExclusivePopover();
      const second = useExclusivePopover();
      return { first, second };
    });

    act(() => result.current.first.onOpenChange(true));
    result.current.first.popupRef.current = document.createElement("div");
    act(() => result.current.first.onOpenChange(false));
    act(() => result.current.first.onOpenChange(true));
    act(() => result.current.first.onOpenChangeComplete(false));
    expect(result.current.first.open).toBe(true);

    act(() => result.current.second.onOpenChange(true));
    expect(result.current.first.open).toBe(false);
    expect(result.current.second.open).toBe(false);
    act(() => result.current.first.onOpenChangeComplete(false));
    expect(result.current.second.open).toBe(true);
    unmount();
  });

  it("cancels a queued replacement when the active owner reopens", () => {
    const { result, unmount } = renderHook(() => {
      const first = useExclusivePopover();
      const second = useExclusivePopover();
      return { first, second };
    });
    act(() => result.current.first.onOpenChange(true));
    result.current.first.popupRef.current = document.createElement("div");
    act(() => result.current.second.onOpenChange(true));
    act(() => result.current.first.onOpenChange(true));
    act(() => result.current.first.onOpenChange(false));
    act(() => result.current.first.onOpenChangeComplete(false));
    expect(result.current.second.open).toBe(false);
    unmount();
  });

  it("lets C directly supersede queued B", () => {
    const { result, unmount } = renderHook(() => {
      const first = useExclusivePopover();
      const second = useExclusivePopover();
      const third = useExclusivePopover();
      return { first, second, third };
    });
    act(() => result.current.first.onOpenChange(true));
    result.current.first.popupRef.current = document.createElement("div");
    act(() => result.current.second.onOpenChange(true));
    act(() => result.current.third.onOpenChange(true));
    act(() => result.current.first.onOpenChangeComplete(false));
    expect(result.current.second.open).toBe(false);
    expect(result.current.third.open).toBe(true);
    unmount();
  });

  it("promotes pending ownership when the active root unmounts", () => {
    const first = renderHook(() => useExclusivePopover());
    const second = renderHook(() => useExclusivePopover());
    act(() => first.result.current.onOpenChange(true));
    first.result.current.popupRef.current = document.createElement("div");
    act(() => second.result.current.onOpenChange(true));
    expect(second.result.current.open).toBe(false);
    first.unmount();
    expect(second.result.current.open).toBe(true);
    second.unmount();
  });

  it("removes pending ownership when that root unmounts", () => {
    const first = renderHook(() => useExclusivePopover());
    const second = renderHook(() => useExclusivePopover());
    const third = renderHook(() => useExclusivePopover());
    act(() => first.result.current.onOpenChange(true));
    first.result.current.popupRef.current = document.createElement("div");
    act(() => second.result.current.onOpenChange(true));
    second.unmount();
    act(() => first.result.current.onOpenChangeComplete(false));
    act(() => third.result.current.onOpenChange(true));
    expect(third.result.current.open).toBe(true);
    first.unmount();
    third.unmount();
  });
});
