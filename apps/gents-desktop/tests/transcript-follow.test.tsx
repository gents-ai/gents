import { act, fireEvent, renderHook } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { useFollowTail, useOlderPages } from "../src/ui/lib/scroll";

/* jsdom has no ResizeObserver: these stand in for the content box growing */
const observers = new Set<() => void>();
class FakeResizeObserver {
  constructor(private readonly callback: () => void) {}
  observe() {
    observers.add(this.callback);
  }
  disconnect() {
    observers.delete(this.callback);
  }
}
vi.stubGlobal("ResizeObserver", FakeResizeObserver);

function transcriptFixture() {
  const viewport = document.createElement("div");
  viewport.dataset.slot = "scroll-area-viewport";
  viewport.append(document.createElement("div"));

  let scrollHeight = 500;
  Object.defineProperties(viewport, {
    clientHeight: { configurable: true, get: () => 200 },
    scrollHeight: { configurable: true, get: () => scrollHeight },
  });

  return {
    viewport,
    /* the content box grows, by a new row or by text revealed in place */
    growTo(height: number) {
      scrollHeight = height;
      act(() => observers.forEach((notify) => notify()));
    },
    setHeight(height: number) {
      scrollHeight = height;
    },
  };
}

describe("transcript streaming follow", () => {
  it("stays pinned across growth, releases on scroll up, and relocks at the tip", () => {
    const fixture = transcriptFixture();
    const { result } = renderHook(() => useFollowTail(fixture.viewport, "session-1"));

    expect(fixture.viewport.scrollTop).toBe(500);
    expect(result.current.atBottom).toBe(true);

    // The model appends a chunk larger than the proximity threshold. Follow is
    // based on the reader's prior intent, not the newly increased height.
    fixture.growTo(900);
    expect(fixture.viewport.scrollTop).toBe(900);

    act(() => {
      fixture.viewport.scrollTop = 100;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(result.current.atBottom).toBe(false);

    fixture.growTo(1_200);
    expect(fixture.viewport.scrollTop).toBe(100);

    act(() => {
      fixture.viewport.scrollTop = 1_000;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    expect(result.current.atBottom).toBe(true);

    fixture.growTo(1_500);
    expect(fixture.viewport.scrollTop).toBe(1_500);
  });

  it("lands at the foot of a scroller that mounts after its subject was chosen", () => {
    const fixture = transcriptFixture();
    const { rerender } = renderHook(
      ({ scroller }) => useFollowTail(scroller, "session-1"),
      { initialProps: { scroller: null as HTMLElement | null } },
    );
    fixture.setHeight(800);
    rerender({ scroller: fixture.viewport });
    expect(fixture.viewport.scrollTop).toBe(800);
  });

  it("starts a new subject at its foot even after the reader scrolled up", () => {
    const fixture = transcriptFixture();
    const { rerender } = renderHook(
      ({ subject }) => useFollowTail(fixture.viewport, subject),
      {
        initialProps: { subject: "a" },
      },
    );
    act(() => {
      fixture.viewport.scrollTop = 0;
      fixture.viewport.dispatchEvent(new Event("scroll"));
    });
    fixture.setHeight(700);
    rerender({ subject: "b" });
    expect(fixture.viewport.scrollTop).toBe(700);
  });
});

describe("older transcript pages", () => {
  /* the load queues the older rows as React state; they reach the screen in
     a commit before the load resolves (a store's synchronous update) or with
     the commit that ends loading (a batched one). The row is put back in that
     commit either way. */
  it.each(["before", "with"])(
    "loads on upward navigation only, deduplicates requests and keeps the reader's place when rows land %s the load's end",
    async (order) => {
      const fixture = transcriptFixture();
      fixture.viewport.scrollTop = 300;
      let finish!: () => void;
      let landRows!: () => void;
      const load = vi.fn(async () => {
        if (order === "before") act(landRows);
        await new Promise<void>((resolve) => {
          finish = resolve;
        });
        if (order === "with") landRows();
        return true;
      });
      const { result, unmount } = renderHook(() => {
        const [oldest, setOldest] = useState("row-10");
        landRows = () => {
          fixture.setHeight(900);
          setOldest("row-0");
        };
        return useOlderPages(fixture.viewport, "session-1", true, load, oldest);
      });
      expect(load).not.toHaveBeenCalled();
      act(() => {
        fixture.viewport.scrollTop = 100;
        fixture.viewport.dispatchEvent(new Event("scroll"));
        fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
      });
      expect(load).toHaveBeenCalledTimes(1);
      expect(result.current).toBe(true);
      await act(async () => {
        finish();
        await Promise.resolve();
      });
      expect(fixture.viewport.scrollTop).toBe(500);
      expect(result.current).toBe(false);
      unmount();
    },
  );

  /* a load that resolves at once can have its start and end batched into
     one commit; the next page must still be asked for */
  it("asks for the next page after a load that settled at once", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 0;
    let page = 10;
    let landRows!: () => void;
    const load = vi.fn(async () => {
      landRows();
      return true;
    });
    const { unmount } = renderHook(() => {
      const [oldest, setOldest] = useState(`row-${page}`);
      landRows = () => {
        page -= 1;
        setOldest(`row-${page}`);
      };
      return useOlderPages(fixture.viewport, "session-1", true, load, oldest);
    });
    for (const _ of [1, 2]) {
      await act(async () => {
        fixture.viewport.scrollTop = 0;
        fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
      });
    }
    expect(load).toHaveBeenCalledTimes(2);
    unmount();
  });

  it("leaves the reader where they are when no older rows land", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 100;
    const load = vi.fn(async () => false);
    const { result, unmount } = renderHook(() =>
      useOlderPages(fixture.viewport, "session-1", true, load, "row-10"),
    );
    await act(async () => {
      fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 }));
    });
    expect(load).toHaveBeenCalledTimes(1);
    expect(fixture.viewport.scrollTop).toBe(100);
    expect(result.current).toBe(false);
    unmount();
  });

  it("ignores late scroll correction after switching sessions and stops at history's start", async () => {
    const fixture = transcriptFixture();
    fixture.viewport.scrollTop = 0;
    let finish!: (value: boolean) => void;
    const load = vi.fn(
      () =>
        new Promise<boolean>((resolve) => {
          finish = resolve;
        }),
    );
    const { rerender, unmount } = renderHook(
      ({ subject, older }) =>
        useOlderPages(fixture.viewport, subject, older, load, "row-10"),
      { initialProps: { subject: "a", older: true } },
    );
    act(() => fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 })));
    expect(load).toHaveBeenCalledTimes(1);
    rerender({ subject: "b", older: false });
    await act(async () => {
      fixture.setHeight(900);
      finish(true);
      await Promise.resolve();
    });
    expect(fixture.viewport.scrollTop).toBe(0);
    act(() => fixture.viewport.dispatchEvent(new WheelEvent("wheel", { deltaY: -20 })));
    expect(load).toHaveBeenCalledTimes(1);
    unmount();
  });
});

describe("short transcript upward intent", () => {
  it.each(["touch", "keyboard"])(
    "loads older content through %s at the top",
    async (input) => {
      const fixture = transcriptFixture();
      const load = vi.fn(async () => false);
      const { unmount } = renderHook(() =>
        useOlderPages(fixture.viewport, "a", true, load, "row-10"),
      );
      expect(load).not.toHaveBeenCalled();
      await act(async () => {
        if (input === "touch") {
          fireEvent.touchStart(fixture.viewport, { touches: [{ clientY: 50 }] });
          fireEvent.touchMove(fixture.viewport, { touches: [{ clientY: 80 }] });
        } else {
          fireEvent.keyDown(fixture.viewport, { key: "PageUp" });
        }
      });
      expect(load).toHaveBeenCalledTimes(1);
      unmount();
    },
  );
});
